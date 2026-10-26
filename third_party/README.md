# Third-party code

## libprojectM

[libprojectM](https://github.com/projectM-visualizer/projectm) (LGPL-2.1-or-later), the
MilkDrop renderer behind the Visualizer view. A git submodule pinned to a release tag, with
its own `vendor/projectm-eval` submodule; fetch both with
`git submodule update --init --recursive`.

`crates/lio-projectm` uses it only when pkg-config finds no system libprojectM 4.1 or newer
(or `LIO_PROJECTM_BUNDLED=1` is set). It then builds it with CMake as a static library for
OpenGL ES 3, without the playlist library.

The Flatpak builds the same release as a module of its own (`libprojectM` in
`build-aux/dev.m5ka.Lio*.json`), and Lio links it through pkg-config.

To update, keep these in step:

1. `git -C third_party/libprojectM fetch --tags && git -C third_party/libprojectM checkout <tag>`,
   then `git submodule update --init --recursive`.
2. Point the `tag` and `commit` of the `libprojectM` module in both Flatpak manifests at it.
3. Check the declarations in `crates/lio-projectm/src/lib.rs` against
   `src/api/include/projectM-4/*.h`.

Once libprojectM 4.2 is released, also switch Lio to `projectm_opengl_render_frame_fbo` and
remove the `glBindFramebuffer` workaround in `src/widgets/milkdrop.rs`.
