#!/usr/bin/env bash
# A small LGPL ffmpeg for Wisp's screen sharing, as `wisp-ffmpeg`: only the
# hardware H.264 encoders and decoders, raw video in and out, and the few
# filters gpu.rs and hwdecode.rs use. No GPL or nonfree parts.
#
#   build.sh <rust target triple> <output dir>
#
# Linux: NVENC/NVDEC (loaded at runtime) and VA-API. Windows (cross-compiled
# with MinGW from Linux): NVENC/NVDEC, AMF, Media Foundation, D3D11VA.
# macOS: VideoToolbox.
set -euo pipefail

TARGET=$1
OUT=$(mkdir -p "$2" && cd "$2" && pwd)
FFMPEG=7.1.1
NV_HEADERS=n12.1.14.0
AMF=v1.4.35

work=$(mktemp -d)
cd "$work"
curl -sSfL "https://ffmpeg.org/releases/ffmpeg-$FFMPEG.tar.xz" | tar xJ
prefix=$work/prefix
mkdir -p "$prefix/include"

nv_headers() {
    git clone -q --depth 1 -b "$NV_HEADERS" https://github.com/FFmpeg/nv-codec-headers
    make -s -C nv-codec-headers PREFIX="$prefix" install
    export PKG_CONFIG_PATH="$prefix/lib/pkgconfig"
}

flags=(
    --disable-everything --disable-autodetect --disable-doc --disable-debug --disable-network
    --disable-ffplay --disable-ffprobe --disable-gpl --disable-nonfree --enable-static --disable-shared
    --enable-protocol=pipe,file
    --enable-demuxer=h264,rawvideo --enable-muxer=h264,rawvideo,null --enable-indev=lavfi
    # hevc only for the film grain code h264's SEI parsing links against
    --enable-decoder=h264,hevc,rawvideo,wrapped_avframe --enable-parser=h264 --enable-encoder=rawvideo,wrapped_avframe
    --enable-bsf=h264_metadata
    --enable-filter=scale,pad,format,null,color,hwupload,hwdownload
    --extra-cflags="-I$prefix/include"
)
exe=ffmpeg
case "$TARGET" in
    *-linux-*)
        nv_headers
        flags+=(
            --enable-ffnvcodec --enable-cuda --enable-nvenc --enable-nvdec --enable-vaapi
            --enable-encoder=h264_nvenc,h264_vaapi --enable-hwaccel=h264_nvdec,h264_vaapi
        )
        ;;
    *-windows-*)
        nv_headers
        git clone -q --depth 1 --filter=blob:none --sparse -b "$AMF" https://github.com/GPUOpen-LibrariesAndSDKs/AMF amf
        git -C amf sparse-checkout set amf/public/include
        cp -r amf/amf/public/include "$prefix/include/AMF"
        flags+=(
            --target-os=mingw32 --arch=x86_64 --cross-prefix=x86_64-w64-mingw32- --pkg-config=pkg-config
            --enable-ffnvcodec --enable-cuda --enable-nvenc --enable-nvdec --enable-amf --enable-mediafoundation
            --enable-d3d11va --enable-dxva2
            --enable-encoder=h264_nvenc,h264_amf,h264_mf --enable-hwaccel=h264_nvdec,h264_d3d11va,h264_dxva2
            --extra-ldflags=-static
        )
        exe=ffmpeg.exe
        ;;
    *-apple-darwin)
        arch=${TARGET%%-*}
        # clang calls it arm64
        cc_arch=${arch/aarch64/arm64}
        # the SDK of the selected Xcode, like Cargo's builds use (a stale
        # Command Line Tools SDK can't be linked against)
        SDKROOT=$(xcrun --sdk macosx --show-sdk-path)
        export SDKROOT
        flags+=(
            --enable-videotoolbox --enable-encoder=h264_videotoolbox --enable-hwaccel=h264_videotoolbox
            --arch="$arch" --cc="clang -arch $cc_arch" --extra-ldflags="-arch $cc_arch"
        )
        [ "$arch" = "$(uname -m | sed s/arm64/aarch64/)" ] || flags+=(--enable-cross-compile)
        ;;
    *)
        echo "no ffmpeg build for $TARGET" >&2
        exit 1
        ;;
esac

cd "ffmpeg-$FFMPEG"
./configure "${flags[@]}" >"$work/configure.log" || { tail -30 "$work/configure.log"; exit 1; }
make -s -j"$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu)" "$exe"
case "$exe" in
    *.exe) x86_64-w64-mingw32-strip "$exe"; cp "$exe" "$OUT/wisp-ffmpeg.exe" ;;
    *) strip "$exe" 2>/dev/null || strip -x "$exe"; cp "$exe" "$OUT/wisp-ffmpeg" ;;
esac
ls -l "$OUT"
