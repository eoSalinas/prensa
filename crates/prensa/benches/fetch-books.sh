#!/bin/sh
# Download the books `cargo bench -p prensa` runs on into benches/books/.
#
# The books are free to download but not kept in git (they are large, and Pro
# Git's license forbids commercial use). They are used as published, and each
# one exercises a different part of the engine:
#
#   progit.epub      Pro Git 2.1.450 (CC BY-NC-SA 3.0): 188 PNG diagrams and
#                    screenshots and 13 embedded fonts.
#   oz.epub          The Wonderful Wizard of Oz (Gutenberg #43936, public
#                    domain): 148 color JPEG illustrations.
#   moby-dick.epub   Moby Dick (Gutenberg #2701, public domain): a long text
#                    with one image, so the cost of everything but images.
#
# Needs curl. Gutenberg's main site often refuses scripts, so its books come
# from the pglaf.org mirror, which can be slow. Gutenberg rebuilds its EPUBs
# now and then, so the files (and the timings) can drift a little from the
# baseline in docs/benchmarks.md; the script prints their SHA-256 to compare.
set -eu

books=$(cd "$(dirname "$0")" && pwd)/books
mkdir -p "$books"

fetch() { # $1 = URL, $2 = file name in benches/books/
    if [ ! -f "$books/$2" ]; then
        echo "$2"
        curl -fsSL --retry 3 -A prensa-bench -o "$books/$2.part" "$1"
        mv "$books/$2.part" "$books/$2"
    fi
}

gutenberg() { # $1 = ebook number, $2 = file name
    fetch "https://gutenberg.pglaf.org/cache/epub/$1/pg$1-images-3.epub" "$2"
}

fetch https://github.com/progit/progit2/releases/download/2.1.450/progit.epub progit.epub
gutenberg 43936 oz.epub
gutenberg 2701 moby-dick.epub

cd "$books"
sha256sum progit.epub oz.epub moby-dick.epub
