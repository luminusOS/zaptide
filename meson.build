project(
  'zaptide',
  'rust',
  version: '0.14.0',
  license: 'MIT',
  meson_version: '>= 0.59.0',
)

# Gettext configuration
i18n = import('i18n')
i18n.gettext('zaptide', preset: 'glib')

# Desktop file installation
desktop_file = i18n.merge_file(
  input: 'data/dev.luminusos.ZapTide.desktop.in',
  output: 'dev.luminusos.ZapTide.desktop',
  type: 'desktop',
  po_dir: 'po',
  install: true,
  install_dir: join_paths(get_option('datadir'), 'applications'),
)

# Metainfo file installation
metainfo_file = i18n.merge_file(
  input: 'data/dev.luminusos.ZapTide.metainfo.xml.in',
  output: 'dev.luminusos.ZapTide.metainfo.xml',
  type: 'xml',
  po_dir: 'po',
  install: true,
  install_dir: join_paths(get_option('datadir'), 'metainfo'),
)

# GSettings schema installation
gnome = import('gnome')
gnome.compile_schemas()
install_data(
  'data/dev.luminusos.ZapTide.gschema.xml',
  install_dir: join_paths(get_option('datadir'), 'glib-2.0', 'schemas'),
)

# Cargo build integration
cargo = find_program('cargo', required: true)
cargo_env = environment()
cargo_env.set('CARGO_HOME', join_paths(meson.project_build_root(), 'cargo-home'))

custom_target(
  'cargo-build',
  build_by_default: true,
  build_always_stale: true,
  output: 'zaptide',
  console: true,
  command: [
    cargo,
    'build',
    '--locked',
    '--release',
    '--target-dir', join_paths(meson.project_build_root(), 'target'),
    '&&',
    'cp',
    join_paths(meson.project_build_root(), 'target', 'release', 'zaptide'),
    '@OUTPUT@',
  ],
  install: true,
  install_dir: get_option('bindir'),
)
