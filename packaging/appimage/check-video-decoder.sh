#!/bin/sh
set -eu

plugins="$1/usr/lib/gstreamer-1.0"
if [ ! -s "$plugins/libgstlibav.so" ] && [ ! -s "$plugins/libgstopenh264.so" ]; then
	printf '%s\n' 'AppImage is missing a software H.264 decoder' >&2
	exit 1
fi
for plugin in libgstisomp4.so libgstvideoparsersbad.so; do
	if [ ! -s "$plugins/$plugin" ]; then
		printf 'AppImage is missing %s\n' "$plugin" >&2
		exit 1
	fi
done
