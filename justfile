# Run inside the toolbox that has the GTK4 and libadwaita development files.

default:
    @just --list

# Compiles the translations into target/locale so a build that is not installed finds them.
locale:
    for lang in $(cat po/LINGUAS); do \
        install -d target/locale/$lang/LC_MESSAGES; \
        msgfmt -o target/locale/$lang/LC_MESSAGES/zaptide.mo po/$lang.po; \
    done

run: locale
    ZAPTIDE_LOCALEDIR=target/locale cargo run

# Refreshes po/zaptide.pot and merges it into every translation.
pot:
    grep -rl 'gettext("' src --include=*.rs | sort > po/POTFILES.in
    xgettext --from-code=UTF-8 --language=Rust --keyword=gettext --package-name=zaptide \
        --msgid-bugs-address=https://github.com/luminusOS/zaptide/issues -f po/POTFILES.in -o po/zaptide.pot
    for lang in $(cat po/LINGUAS); do msgmerge --update --backup=none po/$lang.po po/zaptide.pot; done

fmt:
    cargo fmt

# The same checks CI runs.
check:
    cargo fmt --check
    cargo clippy --locked --all-targets -- -D warnings
    cargo test --locked --all-targets

test:
    cargo test

clean:
    cargo clean
