# Ubuntu 24.04 amd64 with a compiler and the shared libraries FileMaker Server
# 26's libSupport.so / libDBEngine.so need. The FileMaker libraries themselves
# are mounted read-only at run time, never copied into the image.
# BASE_IMAGE lets a build pull the same image through a registry mirror.
ARG BASE_IMAGE=ubuntu:24.04
FROM ${BASE_IMAGE}
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends g++ binutils file \
    libicu74 libfontconfig1 libheif1 libzbar0t64 libodbc2 libboost-chrono1.83.0 libboost-thread1.83.0 libboost-system1.83.0 \
    libxml2 libjbig0 liblcms2-2 libtiff6 libjpeg8 liblqr-1-0 libpng16-16t64 libdjvulibre21 libfreetype6 libwebpmux3 libwebpdemux2 libwebp7 \
    libxext6 libxt6t64 libsm6 libice6 libx11-6 libopenexr-3-1-30 libimath-3-1-29t64 libopenjp2-7 libxslt1.1 libcurl4t64 libcurl3t64-gnutls libssh2-1t64 libldap2 libsasl2-2 libetpan20 libelf1t64 \
    && rm -rf /var/lib/apt/lists/*
