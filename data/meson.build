# Desktop file
desktop_conf = configuration_data()
desktop_conf.set('icon', application_id)
desktop_conf.set('app_id', application_id)
desktop_file = i18n.merge_file(
  type: 'desktop',
  input: configure_file(
    input: '@0@.desktop.in'.format(base_id),
    output: '@BASENAME@.in',
    configuration: desktop_conf,
  ),
  output: '@0@.desktop'.format(application_id),
  po_dir: '../po',
  install: true,
  install_dir: datadir / 'applications',
)
if desktop_file_validate.found()
  test('validate-desktop', desktop_file_validate, args: [desktop_file.full_path()], depends: desktop_file)
endif

# MetaInfo
metainfo_conf = configuration_data()
metainfo_conf.set('app_id', application_id)
metainfo_conf.set('gettext_package', gettext_package)
metainfo_file = i18n.merge_file(
  input: configure_file(
    input: '@0@.metainfo.xml.in'.format(base_id),
    output: '@BASENAME@.in',
    configuration: metainfo_conf,
  ),
  output: '@0@.metainfo.xml'.format(application_id),
  po_dir: '../po',
  install: true,
  install_dir: datadir / 'metainfo',
)
if appstreamcli.found()
  test('validate-metainfo', appstreamcli, args: ['validate', '--no-net', '--explain', metainfo_file.full_path()], depends: metainfo_file)
endif

# GSettings schema (installed under the base ID; the Devel build uses the same keys)
install_data('@0@.gschema.xml'.format(base_id), install_dir: datadir / 'glib-2.0' / 'schemas')
test('validate-schema', glib_compile_schemas, args: ['--strict', '--dry-run', meson.current_source_dir()])

# Icons
# App icon PNGs, generated from data/branding/ by build-aux/make-icons.sh
foreach size : [16, 24, 32, 48, 64, 128, 256, 512]
  size_dir = '@0@x@0@'.format(size)
  install_data(
    'icons/hicolor' / size_dir / 'apps' / '@0@.png'.format(application_id),
    install_dir: datadir / 'icons' / 'hicolor' / size_dir / 'apps',
  )
endforeach
install_data(
  'icons/hicolor/symbolic/apps/@0@-symbolic.svg'.format(base_id),
  install_dir: datadir / 'icons' / 'hicolor' / 'symbolic' / 'apps',
  rename: '@0@-symbolic.svg'.format(application_id),
)

# D-Bus service for GApplication activation
service_conf = configuration_data()
service_conf.set('app_id', application_id)
service_conf.set('bindir', bindir)
configure_file(
  input: '@0@.service.in'.format(base_id),
  output: '@0@.service'.format(application_id),
  configuration: service_conf,
  install_dir: datadir / 'dbus-1' / 'services',
)
