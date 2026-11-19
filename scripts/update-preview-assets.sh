#!/bin/sh
# Fetches the third-party scripts the live preview embeds into the binary.
#
# Usage: scripts/update-preview-assets.sh [--print-hashes]
#
# Versions and SHA-256 digests are pinned below. To upgrade, change a version,
# run with --print-hashes, review the upstream release, then paste the new
# digests. The files land in assets/preview/vendor/ next to their licenses.
set -eu

cd "$(dirname "$0")/.."
dest=assets/preview/vendor
mkdir -p "$dest"

MARKDOWN_IT=14.1.0
MARKDOWN_IT_FOOTNOTE=4.0.0
HIGHLIGHT=11.10.0
MERMAID=11.4.1

# name|url|sha256
assets() {
  cat <<EOF
markdown-it.min.js|https://cdn.jsdelivr.net/npm/markdown-it@$MARKDOWN_IT/dist/markdown-it.min.js|38c70a1e7ca91ab40e2d9e6e60129851a717ed1c7d4acbbdd41bf9503791cf68
markdown-it.LICENSE|https://cdn.jsdelivr.net/npm/markdown-it@$MARKDOWN_IT/LICENSE|792c48c5a849a15fdf9e37e8bcf9e6d1dd13b32b46c642a748a0a46a9919d473
markdown-it-footnote.min.js|https://cdn.jsdelivr.net/npm/markdown-it-footnote@$MARKDOWN_IT_FOOTNOTE/dist/markdown-it-footnote.min.js|d6fee58a3b56c5742fa18f3e01f1d317cc99975683ebd39c9195cb2aff0c2e42
markdown-it-footnote.LICENSE|https://cdn.jsdelivr.net/npm/markdown-it-footnote@$MARKDOWN_IT_FOOTNOTE/LICENSE|81d61236a5f25294b2dad0c866d4ad6a40b865148b28df9a71f49ba771a6c54b
highlight.min.js|https://cdn.jsdelivr.net/npm/@highlightjs/cdn-assets@$HIGHLIGHT/highlight.min.js|471ef9ae90c407af440fcdc48edfeeb562106b3267bd12d99071c162fb52ed32
highlight-github.min.css|https://cdn.jsdelivr.net/npm/@highlightjs/cdn-assets@$HIGHLIGHT/styles/github.min.css|3a9a5def8b9c311e5ae43abde85c63133185eed4f0d9f67fea4b00a8308cf066
highlight-github-dark.min.css|https://cdn.jsdelivr.net/npm/@highlightjs/cdn-assets@$HIGHLIGHT/styles/github-dark.min.css|9f208d022102b1d0c7aebfecd8e42ca7997d5de636649d2b31ea63093d809019
highlight.LICENSE|https://cdn.jsdelivr.net/npm/@highlightjs/cdn-assets@$HIGHLIGHT/LICENSE|6c081431591d9df696c82dc598fe1423765b8a299b200ed00b281afd0f64c490
mermaid.min.js|https://cdn.jsdelivr.net/npm/mermaid@$MERMAID/dist/mermaid.min.js|a43bc1afd446f9c4cc66ac5dd45d02e8d65e26fc5344ec0ef787f88d6ddb6f9e
mermaid.LICENSE|https://cdn.jsdelivr.net/npm/mermaid@$MERMAID/LICENSE|ec9fb67dcb25eccc416ed56e1aab819222c805a2a4bfe4cb19e7556bf2ffde80
EOF
}

print=0
[ "${1:-}" = "--print-hashes" ] && print=1

assets | while IFS='|' read -r name url sha; do
  curl -fsSL --retry 3 -o "$dest/$name.tmp" "$url"
  actual=$(sha256sum "$dest/$name.tmp" | cut -d' ' -f1)
  if [ "$print" = 1 ]; then
    printf '%s %s\n' "$actual" "$name"
  elif [ "$actual" != "$sha" ]; then
    rm -f "$dest/$name.tmp"
    printf 'digest mismatch for %s: %s\n' "$name" "$actual" >&2
    exit 1
  fi
  mv "$dest/$name.tmp" "$dest/$name"
done
