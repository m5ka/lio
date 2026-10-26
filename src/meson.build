cargo_options = ['--locked', '--manifest-path', meson.project_source_root() / 'Cargo.toml']
cargo_options += ['--target-dir', meson.project_build_root() / 'src']

if get_option('profile') == 'default'
  cargo_options += ['--release']
  rust_target = 'release'
  message('Building in release mode')
else
  rust_target = 'debug'
  message('Building in debug mode')
endif

# Respect a CARGO_HOME from the environment (Flatpak builds point it at vendored crates).
cargo_home = run_command('sh', '-c', 'printf %s "${CARGO_HOME:-}"', check: false).stdout().strip()
if cargo_home == ''
  cargo_home = meson.project_build_root() / 'cargo-home'
endif

cargo_env = [
  'CARGO_HOME=' + cargo_home,
  'LIO_APP_ID=' + application_id,
  'LIO_PROFILE=' + profile,
  'LIO_VERSION=' + meson.project_version() + version_suffix,
  'LIO_LOCALEDIR=' + localedir,
  'LIO_PKGDATADIR=' + pkgdatadir,
  'LIO_GETTEXT_PACKAGE=' + gettext_package,
]

blueprint_compiler = find_program('blueprint-compiler', required: false)
if blueprint_compiler.found()
  cargo_env += ['LIO_BLUEPRINT_COMPILER=' + blueprint_compiler.full_path()]
endif

custom_target(
  'cargo-build',
  build_by_default: true,
  build_always_stale: true,
  output: meson.project_name(),
  console: true,
  install: true,
  install_dir: bindir,
  command: [
    'env',
    cargo_env,
    cargo,
    'build',
    cargo_options,
    '&&',
    'cp',
    'src' / rust_target / meson.project_name(),
    '@OUTPUT@',
  ],
)

test(
  'cargo-test',
  cargo,
  args: ['test', '--workspace', cargo_options],
  env: cargo_env,
  timeout: 600,
)
