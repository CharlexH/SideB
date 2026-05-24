FROM --platform=linux/arm64 debian:bullseye-slim AS builder

ARG FFMPEG_VERSION=7.1.1
ARG LAME_VERSION=3.100
ARG TARGET_TRIPLE=aarch64-linux-gnu

ENV DEBIAN_FRONTEND=noninteractive
ENV PREFIX=/opt/sideb-ffmpeg
ENV PKG_CONFIG_PATH=/opt/sideb-ffmpeg/lib/pkgconfig

RUN apt-get update -o Acquire::Retries=5 \
 && apt-get install -y --fix-missing --no-install-recommends -o Acquire::Retries=5 \
    autoconf \
    automake \
    build-essential \
    ca-certificates \
    curl \
    file \
    libtool \
    make \
    nasm \
    pkg-config \
    xz-utils \
    yasm \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /build

RUN curl -fsSLo lame.tar.gz "https://downloads.sourceforge.net/project/lame/lame/${LAME_VERSION}/lame-${LAME_VERSION}.tar.gz" \
 && tar -xzf lame.tar.gz \
 && cd "lame-${LAME_VERSION}" \
 && ./configure \
      --prefix="${PREFIX}" \
      --enable-static \
      --disable-shared \
      --disable-frontend \
 && make -j"$(nproc)" \
 && make install

RUN curl -fsSLo ffmpeg.tar.xz "https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VERSION}.tar.xz" \
 && tar -xf ffmpeg.tar.xz \
 && cd "ffmpeg-${FFMPEG_VERSION}" \
 && ./configure \
      --prefix="${PREFIX}" \
      --arch=aarch64 \
      --target-os=linux \
      --pkg-config=pkg-config \
      --pkg-config-flags=--static \
      --extra-cflags="-I${PREFIX}/include" \
      --extra-ldflags="-L${PREFIX}/lib" \
      --extra-libs="-lm -ldl -lpthread" \
      --disable-autodetect \
      --disable-debug \
      --disable-doc \
      --disable-ffplay \
      --disable-ffprobe \
      --disable-network \
      --disable-everything \
      --enable-ffmpeg \
      --enable-static \
      --disable-shared \
      --enable-protocol=file,pipe \
      --enable-libmp3lame \
      --enable-avcodec \
      --enable-avfilter \
      --enable-avformat \
      --enable-swresample \
      --enable-filter=aresample,aformat \
      --enable-parser=aac,flac,mpegaudio,opus,vorbis \
      --enable-demuxer=aac,flac,matroska,mov,mp3,ogg,wav \
      --enable-decoder=aac,alac,flac,mp3,opus,pcm_s16le,pcm_s24le,vorbis \
      --enable-muxer=mp3,mov,mp4,ipod,pcm_s16le \
      --enable-encoder=libmp3lame,pcm_s16le \
 && make -j"$(nproc)" \
 && make install

RUN "${PREFIX}/bin/ffmpeg" -hide_banner -encoders > /tmp/ffmpeg-encoders.txt 2>/dev/null \
 && grep -q 'libmp3lame' /tmp/ffmpeg-encoders.txt \
 && grep -q 'pcm_s16le' /tmp/ffmpeg-encoders.txt \
 && "${PREFIX}/bin/ffmpeg" -hide_banner -muxers > /tmp/ffmpeg-muxers.txt 2>/dev/null \
 && grep -q 's16le' /tmp/ffmpeg-muxers.txt \
 && "${PREFIX}/bin/ffmpeg" -hide_banner -h muxer=s16le >/dev/null 2>&1

RUN mkdir -p /out \
 && cp "${PREFIX}/bin/ffmpeg" /out/ffmpeg-lite \
 && sha256sum /out/ffmpeg-lite > /out/ffmpeg-lite.sha256 \
 && printf 'FFmpeg=%s\nLAME=%s\nTarget=%s\n' "${FFMPEG_VERSION}" "${LAME_VERSION}" "${TARGET_TRIPLE}" > /out/ffmpeg-build-info.txt

FROM scratch AS artifact
COPY --from=builder /out/ /out/
