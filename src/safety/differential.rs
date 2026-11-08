#[cfg(test)]
mod targeted {
    #[test]
    fn targeted_full_render() {
        let fixtures = [
            "hello *bold* & <tag> www.example.com/path?x=1&y=2",
            "(www.a(www.b@127.0.0.1))) and https://example.org/test).",
            "whatsapp://send?phone=%31%32%33%34%35%36%37",
            "https://wa.me/1234567890 and https://api.whatsapp.com/send?phone=12345678",
            "www.%ff(www.ok.example) www.[::1]/a",
            "www.%ff((www.é((,))).,)))",
            "www.%ff((www.é((,))).,))) www.。a.%2eé((,)))",
            "> *hi*\n- _é_ www.bücher.de/путь?x=1",
            "`www.example.com` ```\nhttps://example.com\n```",
            "https:///example.com https:////www.example.org/a",
            "http://127.1 http://0x7f.1 https://[::1]/x",
            "https://example.com:443/a?b=2#c",
        ];
        for input in fixtures {
            assert_eq!(
                crate::safety::linkify_markup(input),
                crate::safety::reference::linkify_markup(input),
                "{input:?}"
            );
        }
    }
}

#[cfg(test)]
mod randomized {
    #[test]
    fn synthetic_full_render() {
        let parts = [
            "www.a(",
            "www.b@",
            "http://",
            "https://",
            "whatsapp://send?phone=",
            "%31",
            "12345678",
            "é",
            "。",
            "[::1]",
            ")",
            "!",
            "/x",
            "?q=",
            "&",
            " ",
            "\n",
            "*bold*",
            "`x`",
            "<",
            "@",
            ":80",
            "%ff",
            "a",
        ];
        let mut state = 0x8abced312u64;
        for _ in 0..30000 {
            let mut s = String::new();
            for _ in 0..7 {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                s.push_str(parts[((state >> 32) as usize) % parts.len()]);
            }
            assert_eq!(
                crate::safety::linkify_markup(&s),
                crate::safety::reference::linkify_markup(&s),
                "{s:?}"
            );
        }
    }
}

#[cfg(test)]
mod scaling {
    #[test]
    fn synthetic_rejected_and_consumed_scales() {
        for n in [64usize, 128, 256, 512] {
            for (name, input) in [
                (
                    "rejected",
                    format!("{}%ff{}", "www.a(".repeat(n), ")".repeat(n)),
                ),
                ("accepted", "www.example.com ".repeat(n)),
                (
                    "unicode",
                    format!(
                        "{}@{}{}",
                        "www.a(".repeat(n),
                        "é".repeat(100),
                        ")".repeat(n)
                    ),
                ),
            ] {
                crate::safety::stats::reset();
                let output = crate::safety::linkify_markup(&input);
                let stats = crate::safety::stats::snapshot();
                assert_eq!(
                    output,
                    crate::safety::reference::linkify_markup(&input),
                    "{name} n={n}"
                );
                assert!(
                    stats.consumed_web_bytes <= input.len(),
                    "overlapping consumed ranges: {name} {n}"
                );
                assert!(
                    stats.accepted_parser_bytes
                        <= 2 * stats.consumed_web_bytes + 8 * stats.consumed_web_links,
                    "unexpected accepted parser input: {name} {n}"
                );
                println!("{name} n={n} input={} {stats:?}", input.len());
            }
        }
    }
}

#[cfg(test)]
mod whatsapp_work_bounds {
    #[test]
    fn eager_nested_authority_and_parser_inputs_are_linear_on_synthetic_cases() {
        for n in [64usize, 256, 1024, 4096] {
            let input = format!(
                "{}whatsapp://send?phone=%31%32%33%34%35%36%37",
                "whatsapp://x(".repeat(n)
            );
            crate::safety::stats::reset();
            let output = crate::safety::linkify_markup(&input);
            let w = crate::safety::stats::snapshot();
            assert_eq!(output, crate::safety::reference::linkify_markup(&input));
            assert!(
                w.whatsapp_authority_scanned <= input.len() + 11 * w.candidates,
                "{w:?}"
            );
            assert!(
                w.whatsapp_parser_input_bytes
                    <= w.whatsapp_authority_scanned + 12 * w.whatsapp_parser_calls,
                "{w:?}"
            );
            assert!(w.whatsapp_parser_calls <= w.candidates, "{w:?}");
            eprintln!("nested_whatsapp n={n} input={} stats={w:?}", input.len());
        }
    }
}

#[cfg(test)]
mod compact_path_tests {
    #[test]
    fn checkpoint_path_matches_dense_states_at_block_boundaries() {
        for n in [0, 1, 2, 60, 61, 62, 63, 64, 65, 66, 126, 127, 128, 129] {
            for segment in [
                "a", ".", "..", "%2e", "%2e.", ".%2e", "%2e%2e", "é", "", "abc%20",
            ] {
                for before in ["/", "/a/", "/../", "?/a/"] {
                    let input =
                        format!("{before}{}{segment}/../x/{}", "a".repeat(n), ")".repeat(n));
                    assert!(url_batch::batch::test_compact_path(&input), "{input:?}");
                }
            }
        }
    }
}
