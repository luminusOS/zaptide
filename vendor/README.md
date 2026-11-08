# Local rendering dependencies

These are local modifications of published crate sources, not new upstream releases. Keep their upstream versions and license notices intact.

`absolute-url` is `url` 2.5.8 from rust-url, whose crate metadata records upstream revision `d6ea13c5f8e7e6e627f6390161b3e185bda5e5ce`. Its `batch-research` feature adds indexed authority/path queries and factors parser predicates into shared helpers. The ordinary registry `url` remains the application's public URL type and canonical serializer. `batch-stats` enables test accounting only; normal application builds do not enable it.

`idna-1.1.0` is `idna` 1.1.0 from rust-url. The published crate records revision `91377f48bf35011d042aa5abef9e7f2a0a625aaa` with a dirty-tree flag; that upstream metadata is preserved, not evidence of an unmodified local tree. `research-batch` adds URL-policy fragment and actual-tail preparation while retaining the pinned UTS-46 mapper, normalization, Punycode and bidi rules. Keep mapped dots distinct from inserted fragment boundaries and retain both retry preparations until a decision finishes.

`icu_normalizer` is ICU4X 2.3.0, upstream revision `a14f2dad852be26bad277ae704abf27a15cbfed1`. `experimental_linear_ccc` replaces combining-class sorting with stable counting sort and reusable scratch storage. The root Cargo patch applies this same-version fork to the complete dependency graph so ordinary and batch IDNA use the same normalization implementation.

The application keeps the adapter private to `safety`. Provider coverage depends on candidate intervals produced by `LinkScan`; the provider is not a total API for arbitrary intervals. Accepted, consumed links still use the ordinary URL parser, and click-time scheme checks remain separate. No input-length or nesting limit is introduced.

URL and IDNA retain their MIT and Apache-2.0 notices. ICU retains its Unicode License V3 notice. `packaging/install-licenses.sh` copies these notices with the application license into binary distributions; the portable archive carries the same tree into the Flatpak bundle. Run `python3 packaging/test-licenses.py` to check these paths without building or publishing a release.

When updating these forks, compare against both the published crate and these local changes, preserve parser equivalence, and rerun exact-output, adversarial scaling and additional-peak-memory comparisons against the pristine renderer. Normalization expansion, percent-decoding alignment, bidi labels, mapped dots, multiple terminal fragments, both retry keys and four simultaneously live formatting frames are regression boundaries. Test-only counters and the pristine oracle must remain excluded from production builds.
