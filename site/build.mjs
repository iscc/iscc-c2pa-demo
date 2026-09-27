// Build the landing page into _site/: fill the download links in site/index.html from the latest
// GitHub release (prereleases and drafts excluded) and copy the static files, the fonts and the
// logos next to it. Without a release, or offline, the links point at the releases page.
// Usage: node site/build.mjs   (GITHUB_REPOSITORY and GITHUB_TOKEN are optional)

import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const out = join(root, "_site");
const repo = process.env.GITHUB_REPOSITORY || "iscc/iscc-c2pa-demo";
const releasesUrl = `https://github.com/${repo}/releases`;

/** Release assets the page links to, by the file name suffix the release workflow gives them. */
const ASSETS = {
  windows: /-windows-x64-setup\.exe$/,
  macos: /-macos-universal\.dmg$/,
  appimage: /-linux-x86_64\.AppImage$/,
  deb: /-linux-amd64\.deb$/,
  rpm: /-linux-x86_64\.rpm$/,
  sums: /^SHA256SUMS$/,
};

/** Fetch the latest published release, or null when there is none or GitHub is unreachable. */
async function latestRelease() {
  const headers = { Accept: "application/vnd.github+json" };
  if (process.env.GITHUB_TOKEN) headers.Authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
  try {
    const res = await fetch(`https://api.github.com/repos/${repo}/releases/latest`, { headers });
    if (res.ok) return await res.json();
    console.warn(`no latest release (${res.status}); linking the releases page`);
  } catch (e) {
    console.warn(`GitHub unreachable (${e.message}); linking the releases page`);
  }
  return null;
}

/** File size as shown next to a download, e.g. "12.3 MB". */
function formatSize(bytes) {
  return `${(bytes / 1e6).toFixed(1)} MB`;
}

/** Placeholder values for the page: version, date and one url/name/size triple per asset. */
function fields(release) {
  const values = {
    version_label: release ? `${release.tag_name} · ${release.published_at.slice(0, 10)}` : "First release in preparation",
    release_url: release ? release.html_url : releasesUrl,
    releases_url: releasesUrl,
  };
  for (const [key, pattern] of Object.entries(ASSETS)) {
    const asset = release?.assets.find((a) => pattern.test(a.name));
    values[`${key}_url`] = asset ? asset.browser_download_url : releasesUrl;
    values[`${key}_name`] = asset ? asset.name : "See all releases";
    values[`${key}_size`] = asset ? formatSize(asset.size) : "";
  }
  return values;
}

/** Replace every {{key}} in the template; an unknown key is a build error. */
function render(template, values) {
  return template.replace(/\{\{(\w+)\}\}/g, (_, key) => {
    if (!(key in values)) throw new Error(`unknown placeholder {{${key}}}`);
    return values[key];
  });
}

const release = await latestRelease();
rmSync(out, { recursive: true, force: true });
mkdirSync(join(out, "assets"), { recursive: true });
cpSync(join(root, "site/assets"), join(out, "assets"), { recursive: true });
cpSync(join(root, "src/assets/fonts"), join(out, "assets/fonts"), { recursive: true });
for (const logo of ["iscc-logo-black-coral.svg", "favicon.svg"]) {
  cpSync(join(root, "src/assets", logo), join(out, "assets", logo));
}
cpSync(join(root, "site/style.css"), join(out, "style.css"));
cpSync(join(root, "site/CNAME"), join(out, "CNAME"));
const html = render(readFileSync(join(root, "site/index.html"), "utf8"), fields(release));
writeFileSync(join(out, "index.html"), html);
console.log(`built ${out} for ${release ? release.tag_name : "no release"}`);
