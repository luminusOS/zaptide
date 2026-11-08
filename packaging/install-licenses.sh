#!/bin/sh
set -eu

dest=${1:?license destination required}
install -Dm644 LICENSE "$dest/LICENSE"
for path in \
	vendor/absolute-url/LICENSE-APACHE \
	vendor/absolute-url/LICENSE-MIT \
	vendor/idna-1.1.0/LICENSE-APACHE \
	vendor/idna-1.1.0/LICENSE-MIT \
	vendor/icu_normalizer/LICENSE; do
	install -Dm644 "$path" "$dest/$path"
done
