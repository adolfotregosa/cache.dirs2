#!/bin/bash
# Builds the static binary and the Slackware package, then writes
#   ../packages/cache.dirs2-<version>-x86_64-1.txz
#   ../cache.dirs2.plg
# Needs cargo and static glibc (libc6-dev).
set -euo pipefail
cd "$(dirname "$0")"
VERSION=${VERSION:-$(date +%Y.%m.%d)}
NAME=cache.dirs2
PKG=$NAME-$VERSION-x86_64-1
OUT=..

echo "==> building cachedirs2 $VERSION"
( cd rust && cargo test --quiet && \
  CD2_VERSION=$VERSION cargo build --release --target x86_64-unknown-linux-gnu )

rm -rf build && mkdir -p build "$OUT/packages"
cp -a plugin/src/. build/
mkdir -p build/usr/local/emhttp/plugins/$NAME/bin
install -m 0755 rust/target/x86_64-unknown-linux-gnu/release/cachedirs2 build/usr/local/emhttp/plugins/$NAME/bin/cachedirs2
find build -type d -exec chmod 0755 {} +
find build -type f -exec chmod 0644 {} +
chmod 0755 build/usr/local/emhttp/plugins/$NAME/bin/cachedirs2 \
           build/usr/local/emhttp/plugins/$NAME/scripts/* \
           build/usr/local/emhttp/plugins/$NAME/event/*

echo "==> packaging $PKG.txz"
# same layout makepkg produces: "./" first, then paths without the "./" prefix
( cd build && find . | LC_COLLATE=C sort | sed '2,$s,^\./,,' | \
  tar --no-recursion --owner=0 --group=0 -T - -cJf "../$OUT/packages/$PKG.txz" )
rm -rf build

# keep only the package just built (deleted only after the new one exists)
for f in "$OUT"/packages/$NAME-*.txz; do
  [[ -e $f && $(basename "$f") != "$PKG.txz" ]] && rm -f "$f" && echo "==> removed old $(basename "$f")"
done

sed -e "s/@VERSION@/$VERSION/" plugin/$NAME.plg.in > "$OUT/$NAME.plg"
echo "==> done: packages/$PKG.txz and $NAME.plg"
echo "    remember to add a ###$VERSION entry to CHANGES in src/plugin/$NAME.plg.in"
