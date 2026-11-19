# The environment tools/package-ubuntu.sh builds and packages in: Ubuntu
# 24.04 with WebKitGTK's development files for om-gui's web viewers,
# dpkg-dev for dpkg-shlibdeps, and Xvfb and Mesa to start om-gui once and
# record the libraries it opens at run time. The Rust toolchain is not in
# the image: build.sh installs the one rust-toolchain.toml names into the
# mounted target/ folder, where it persists between runs.
FROM ubuntu:24.04
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends \
    build-essential pkg-config ca-certificates curl dpkg-dev file \
    libwebkit2gtk-4.1-dev libgtk-3-dev \
    xvfb xauth libgl1-mesa-dri libegl-mesa0 mesa-vulkan-drivers
# What om-gui opens at run time that the packages above do not bring, found
# by build.sh's probe (which names any other it looks for).
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends libxkbcommon-x11-0
