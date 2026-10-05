//! Semantic-Code Text as iscc-sct 0.2.2 computes it: the text split into overlapping chunks of at
//! most 127 tokens by text-splitter (the crate behind iscc-sct's `semantic-text-splitter`), each
//! chunk embedded by the multilingual MiniLM model and mean-pooled over its tokens, the chunk
//! vectors averaged. Whitespace moves chunk boundaries, so the text is used exactly as extracted.
//!
//! The chunks are embedded in parallel, one per logical core and each on a single thread, while
//! the splitter is still cutting the text: rten spreads one chunk of 128 tokens poorly over many
//! cores. Every chunk's embedding is the same as one at a time, and the mean adds them in text
//! order, so the code does not depend on the number of cores.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Result};
use rten::{Model, RunOptions, ThreadPool};
use rten_tensor::prelude::*;
use rten_tensor::{NdTensor, Tensor};
use text_splitter::{ChunkConfig, ChunkSizer, TextSplitter};
use tokenizers::Tokenizer;

use crate::tools::Cancelled;
use crate::video::Progress;

/// Most tokens per chunk, as iscc-sct's `max_tokens`.
pub const MAX_TOKENS: usize = 127;
/// Most tokens two neighbouring chunks share, as iscc-sct's `overlap`.
pub const OVERLAP: usize = 48;
/// Size of a token and of a chunk embedding.
const DIM: usize = 384;
/// Characters from any position to the next paragraph or line break beyond which iscc-sct
/// switches to its guarded sizer (`SPLIT_GUARD_GAP`).
const SPLIT_GUARD_GAP: usize = 8192;

/// Memo key of the text.
pub(super) fn key(text: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"text");
    hasher.update(text.as_bytes());
    *hasher.finalize().as_bytes()
}

/// A chunk for a worker: its index, the byte offset where it ends, its text.
type Job<'t> = (usize, usize, &'t str);

/// A chunk embedded by a worker: its index, the byte offset where it ends, its embedding.
type Embedded = (usize, usize, Result<Vec<f32>>);

/// The document embedding of `text` by the text model at `model` with the tokenizer at
/// `tokenizer`: the mean of its chunk embeddings, L2-normalised, with one worker per logical
/// core. `progress` hears the share of the text embedded after each chunk and stops the run by
/// returning false.
pub(super) fn embedding(
    model: &Path,
    tokenizer: &Path,
    text: &str,
    progress: Progress,
) -> Result<Vec<f32>> {
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get());
    embedding_with(model, tokenizer, text, workers, progress)
}

/// [`embedding`] with `workers` chunks embedded at a time.
fn embedding_with(
    model: &Path,
    tokenizer: &Path,
    text: &str,
    workers: usize,
    progress: Progress,
) -> Result<Vec<f32>> {
    let encoder =
        Tokenizer::from_file(tokenizer).map_err(|e| anyhow!("cannot load the tokenizer: {e}"))?;
    let sizer = TokenSizer::new(&encoder, text, MAX_TOKENS)?;
    let config = chunk_config(&sizer, MAX_TOKENS, OVERLAP)?;
    let model = Model::load_file(model)?;
    let vectors = embed_chunks(&model, &encoder, config, text, workers, progress)?;
    if vectors.is_empty() {
        bail!("no text to embed");
    }
    // In text order, as iscc-sct's mean adds them, whichever worker finished first.
    let mut sum = vec![0f32; DIM];
    for vector in &vectors {
        sum.iter_mut().zip(vector).for_each(|(s, v)| *s += v);
    }
    let n = vectors.len() as f32;
    Ok(normalized(sum.into_iter().map(|s| s / n).collect()))
}

/// The embeddings of the chunks `config` cuts from `text`, in text order: one thread splits and
/// hands each chunk to `workers` threads as soon as it is cut, while the calling thread gathers
/// the results and reports progress. A failed chunk or a stop ends the run.
fn embed_chunks(
    model: &Model,
    encoder: &Tokenizer,
    config: ChunkConfig<&TokenSizer>,
    text: &str,
    workers: usize,
    progress: Progress,
) -> Result<Vec<Vec<f32>>> {
    let stop = AtomicBool::new(false);
    // A short queue: the splitter stays a few chunks ahead of the workers.
    let (job_tx, job_rx) = mpsc::sync_channel(workers);
    let job_rx = Mutex::new(job_rx);
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| split(config, text, &stop, job_tx));
        for _ in 0..workers.max(1) {
            let done_tx = done_tx.clone();
            scope.spawn(|| work(model, encoder, &job_rx, &stop, done_tx));
        }
        drop(done_tx);
        gather(done_rx, text.len(), &stop, progress)
    })
}

/// Cuts `text` into chunks and queues them for the workers, until the text ends or `stop` is
/// set.
fn split<'t>(
    config: ChunkConfig<&TokenSizer>,
    text: &'t str,
    stop: &AtomicBool,
    jobs: SyncSender<Job<'t>>,
) {
    let splitter = TextSplitter::new(config);
    for (index, (offset, chunk)) in splitter.chunk_indices(text).enumerate() {
        if stop.load(Ordering::Relaxed) || jobs.send((index, offset + chunk.len(), chunk)).is_err()
        {
            break;
        }
    }
}

/// A worker: embeds queued chunks on a thread pool of its own with a single thread, until the
/// queue closes. After a stop it only empties the queue, so the splitter never waits on a full
/// one.
fn work(
    model: &Model,
    encoder: &Tokenizer,
    jobs: &Mutex<Receiver<Job<'_>>>,
    stop: &AtomicBool,
    done: Sender<Embedded>,
) {
    let threads = Arc::new(ThreadPool::with_num_threads(1));
    loop {
        let job = jobs.lock().unwrap_or_else(|e| e.into_inner()).recv();
        let Ok((index, end, chunk)) = job else {
            return;
        };
        if !stop.load(Ordering::Relaxed) {
            // The receiver is gone only after a stop.
            let _ = done.send((index, end, embed_chunk(model, encoder, chunk, &threads)));
        }
    }
}

/// The embeddings from the workers in text order; reports the share of the text up to the
/// furthest chunk embedded. Sets `stop` when a chunk fails or `progress` returns false.
fn gather(
    done: Receiver<Embedded>,
    length: usize,
    stop: &AtomicBool,
    progress: Progress,
) -> Result<Vec<Vec<f32>>> {
    let mut vectors: Vec<Option<Vec<f32>>> = Vec::new();
    let mut reached = 0;
    for (index, end, vector) in done {
        let vector = vector.inspect_err(|_| stop.store(true, Ordering::Relaxed))?;
        if vectors.len() <= index {
            vectors.resize(index + 1, None);
        }
        vectors[index] = Some(vector);
        reached = reached.max(end);
        if !progress(Some(reached as f64 / length as f64)) {
            stop.store(true, Ordering::Relaxed);
            return Err(Cancelled.into());
        }
    }
    Ok(vectors.into_iter().flatten().collect())
}

/// The embedding of one chunk: tokenized as `tokenizer.json` declares (special tokens added,
/// truncated to 128 tokens), run through the model on `threads`, its token vectors averaged under
/// the attention mask and L2-normalised.
fn embed_chunk(
    model: &Model,
    encoder: &Tokenizer,
    chunk: &str,
    threads: &Arc<ThreadPool>,
) -> Result<Vec<f32>> {
    let encoding = encoder
        .encode(chunk, true)
        .map_err(|e| anyhow!("cannot tokenize the text: {e}"))?;
    let as_i32 = |values: &[u32]| values.iter().map(|v| *v as i32).collect::<Vec<i32>>();
    let mask = as_i32(encoding.get_attention_mask());
    let n = mask.len();
    // rten has no int64 tensors; token ids fit into i32.
    let inputs = vec![
        (
            model.node_id("input_ids")?,
            NdTensor::from_data([1, n], as_i32(encoding.get_ids())).into(),
        ),
        (
            model.node_id("attention_mask")?,
            NdTensor::from_data([1, n], mask.clone()).into(),
        ),
        (
            model.node_id("token_type_ids")?,
            NdTensor::from_data([1, n], as_i32(encoding.get_type_ids())).into(),
        ),
    ];
    let options = RunOptions::default().with_thread_pool(Some(threads.clone()));
    let mut outputs = model.run(inputs, &[model.node_id("output")?], Some(options))?;
    let tokens: Tensor<f32> = outputs.remove(0).try_into()?;
    Ok(pool(&tokens.to_vec(), &mask))
}

/// Mean of the token vectors in `flat` (`mask.len()` rows of [`DIM`]) where the mask is set,
/// L2-normalised, as iscc-sct's `attention_pooling`.
fn pool(flat: &[f32], mask: &[i32]) -> Vec<f32> {
    let mut sum = vec![0f32; DIM];
    for (row, m) in flat.as_chunks::<DIM>().0.iter().zip(mask) {
        let m = *m as f32;
        sum.iter_mut().zip(row).for_each(|(s, v)| *s += v * m);
    }
    let count = (mask.iter().sum::<i32>() as f32).max(1e-9);
    normalized(sum.into_iter().map(|s| s / count).collect())
}

/// `v` divided by its L2 norm (at least 1e-9).
fn normalized(v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    v.into_iter().map(|x| x / norm).collect()
}

/// Splits `text` into chunks of at most `max_tokens` tokens (the capacity `sizer` was made for),
/// neighbours sharing at most `overlap`, whitespace kept, as iscc-sct's `split_text`; returns
/// each chunk with its byte offset.
pub fn chunks<'t>(
    sizer: &TokenSizer,
    text: &'t str,
    max_tokens: usize,
    overlap: usize,
) -> Result<Vec<(usize, &'t str)>> {
    let config = chunk_config(sizer, max_tokens, overlap)?;
    Ok(TextSplitter::new(config).chunk_indices(text).collect())
}

/// text-splitter's settings for iscc-sct's chunks: `max_tokens` counted by `sizer`, `overlap`
/// shared, whitespace kept.
fn chunk_config(
    sizer: &TokenSizer,
    max_tokens: usize,
    overlap: usize,
) -> Result<ChunkConfig<&TokenSizer>> {
    Ok(ChunkConfig::new(max_tokens)
        .with_sizer(sizer)
        .with_overlap(overlap)?
        .with_trim(false))
}

/// Code point offsets of the byte offsets `bytes` into `text`, as iscc-sct reports them.
pub fn char_offsets(text: &str, bytes: &[usize]) -> Vec<usize> {
    bytes.iter().map(|b| text[..*b].chars().count()).collect()
}

/// Counts tokens as iscc-sct's chunking tokenizer does: `tokenizer.json` without truncation and
/// padding, no special tokens. For text with long runs between line breaks it is iscc-sct's
/// guarded sizer, which stops counting once a long chunk is plainly too big; the chunks are the
/// same, only sizing such text is no longer quadratic.
pub struct TokenSizer {
    tokenizer: Tokenizer,
    guarded: bool,
    /// Chunk capacity in tokens, which the guarded count must exceed.
    max_tokens: usize,
}

impl TokenSizer {
    /// The sizer iscc-sct picks for `text` and chunks of `max_tokens`, made from the embedding
    /// tokenizer `encoder`.
    pub fn new(encoder: &Tokenizer, text: &str, max_tokens: usize) -> Result<Self> {
        let mut tokenizer = encoder.clone();
        tokenizer
            .with_truncation(None)
            .map_err(|e| anyhow!("cannot set up the tokenizer: {e}"))?;
        tokenizer.with_padding(None);
        Ok(TokenSizer {
            tokenizer,
            guarded: needs_split_guard(text),
            max_tokens,
        })
    }

    /// Tokens of `text` as text-splitter's own Hugging Face sizer counts them (`encode_fast`
    /// without special tokens); without padding and truncation that is every token. That sizer
    /// needs text-splitter's `tokenizers` feature, which forces the Oniguruma C library on
    /// tokenizers; `tokenizer.json` uses no regular expressions. Unigram encodes any text, so
    /// the error case never comes up; it would count as too long for any chunk.
    fn count(&self, text: &str) -> usize {
        self.tokenizer
            .encode_fast(text, false)
            .map_or(usize::MAX, |encoding| encoding.len())
    }

    /// iscc-sct's `token_count_guarded`: for text far longer than a chunk, the tokens of a
    /// prefix up to its last whitespace plus the characters after it, once that prefix alone
    /// exceeds the chunk capacity; else the exact count.
    fn count_guarded(&self, text: &str) -> usize {
        let probe_chars = self.max_tokens * 10;
        let length = text.chars().count();
        if length > probe_chars * 2 {
            let cut = text
                .char_indices()
                .take(probe_chars)
                .enumerate()
                .filter(|(_, (_, c))| is_python_space(*c))
                .last()
                .map(|(chars, (bytes, _))| (chars, bytes));
            if let Some((chars, bytes)) = cut.filter(|(chars, _)| *chars > 0) {
                let prefix = self.count(&text[..bytes]);
                if prefix > self.max_tokens {
                    return prefix + length - chars;
                }
            }
        }
        self.count(text)
    }
}

impl ChunkSizer for TokenSizer {
    fn size(&self, chunk: &str) -> usize {
        if self.guarded {
            self.count_guarded(chunk)
        } else {
            self.count(chunk)
        }
    }
}

/// Whitespace as Python's `\s` matches it in a str: Unicode White_Space plus the information
/// separators U+001C to U+001F.
fn is_python_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// iscc-sct's `needs_split_guard`: whether some span between paragraph breaks (of any level) or
/// between line breaks is longer than [`SPLIT_GUARD_GAP`] characters.
fn needs_split_guard(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let runs = newline_runs(&chars);
    let mut levels: Vec<usize> = runs.iter().map(|r| r.0).collect();
    levels.sort_unstable();
    levels.dedup();
    for min_level in levels {
        let mut pos = 0;
        for &(_, start, end) in runs.iter().filter(|r| r.0 >= min_level) {
            if start - pos > SPLIT_GUARD_GAP {
                return true;
            }
            pos = end;
        }
    }
    let mut pos = 0;
    for (i, c) in chars.iter().enumerate() {
        if is_newline(*c) {
            if i - pos > SPLIT_GUARD_GAP {
                return true;
            }
            pos = i + 1;
        }
    }
    chars.len() - pos > SPLIT_GUARD_GAP
}

/// Runs of two or more `\r`/`\n` whose level (characters, a `\r\n` counting once) is at least
/// two, as `(level, start, end)` in characters.
fn newline_runs(chars: &[char]) -> Vec<(usize, usize, usize)> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !is_newline(chars[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_newline(chars[i]) {
            i += 1;
        }
        let run = &chars[start..i];
        let crlf = run.windows(2).filter(|w| w == &['\r', '\n']).count();
        let level = run.len() - crlf;
        if run.len() >= 2 && level >= 2 {
            runs.push((level, start, i));
        }
    }
    runs
}

/// A line break character.
fn is_newline(c: char) -> bool {
    c == '\r' || c == '\n'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pooling_averages_masked_tokens_and_normalises() {
        let mut flat = vec![0f32; 4 * DIM];
        flat[0] = 3.0; // token 0
        flat[DIM] = 1.0; // token 1
        flat[DIM + 1] = 4.0;
        flat[2 * DIM + 2] = 100.0; // masked out
        let v = pool(&flat, &[1, 1, 0, 0]);
        // Mean (2, 2, 0, ...), normalised.
        let half = 0.5f32.sqrt();
        assert!(
            (v[0] - half).abs() < 1e-6 && (v[1] - half).abs() < 1e-6,
            "{:?}",
            &v[..3]
        );
        assert_eq!(v[2], 0.0);
    }

    #[test]
    fn split_guard_finds_long_spans() {
        assert!(!needs_split_guard("short\n\nparagraphs\n\nonly"));
        let long_line = "word ".repeat(2000);
        assert!(
            needs_split_guard(&long_line),
            "no line break for 10,000 characters"
        );
        // Line breaks everywhere, but paragraph breaks far apart.
        let lines = "a line of text\n".repeat(600);
        assert!(needs_split_guard(&format!("{lines}\n\n{lines}")));
        assert!(!needs_split_guard(&"a line of text\n\n".repeat(600)));
        // A CRLF pair counts as one break: \r\n\r\n is a paragraph break (level 2).
        assert_eq!(newline_runs(&['\r', '\n', '\r', '\n']), [(2, 0, 4)]);
        assert!(newline_runs(&['a', '\r', '\n', 'b']).is_empty());
    }

    #[test]
    fn char_offsets_count_code_points() {
        let text = "Iñtërnâtiônàlizætiøn☃.";
        assert_eq!(char_offsets(text, &[0, 1, 3, 30]), [0, 1, 2, 21]);
    }

    #[test]
    fn python_whitespace_includes_the_separators() {
        assert!(is_python_space('\u{1f}') && is_python_space('\u{a0}') && is_python_space(' '));
        assert!(!is_python_space('\u{200b}') && !is_python_space('x'));
    }

    #[test]
    fn the_number_of_workers_changes_nothing() {
        crate::tools::tests::ensure_semantic();
        let files = crate::tools::semantic_paths(crate::semantic::SemanticKind::Text).unwrap();
        let (model, tokenizer) = (&files[0], &files[1]);
        let text = include_str!("../../tests/fixtures/demo.txt");
        let shares = std::cell::RefCell::new(Vec::new());
        let one = embedding_with(model, tokenizer, text, 1, &|share| {
            shares.borrow_mut().push(share.unwrap());
            true
        })
        .unwrap();
        let shares = shares.into_inner();
        assert!(shares.len() > 10, "{} chunks", shares.len());
        assert!(
            shares.is_sorted() && shares.last() == Some(&1.0),
            "{shares:?}"
        );
        let many = embedding_with(model, tokenizer, text, 5, &|_| true).unwrap();
        assert_eq!(one, many);
    }
}
