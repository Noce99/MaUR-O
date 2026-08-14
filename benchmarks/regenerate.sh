#!/bin/sh
#
#    Copyright 2026 Enrico Mannocci
#
#    This file is part of map_to_image, derived from OpenOrienteering Mapper.
#
#    This program is free software: you can redistribute it and/or modify
#    it under the terms of the GNU General Public License as published by
#    the Free Software Foundation, either version 3 of the License, or
#    (at your option) any later version.
#
#    This program is distributed in the hope that it will be useful,
#    but WITHOUT ANY WARRANTY; without even the implied warranty of
#    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
#    GNU General Public License for more details.
#
#    You should have received a copy of the GNU General Public License
#    along with this program.  If not, see <http://www.gnu.org/licenses/>.
#
# Renders every benchmark map to benchmarks/expected/, replacing the reference
# images which the benchmark tests compare against.
#
# Only do this when a rendering change is understood and intended: it makes the
# tests pass by definition. Review the differences first, for example with
#     ctest --test-dir build -L benchmark
#
# Usage: benchmarks/regenerate.sh [path-to-map_to_image]

set -eu

here=$(dirname "$0")
tool=${1:-$here/../target/release/map_to_image}

if [ ! -x "$tool" ]
then
	echo "No map_to_image at '$tool'. Build it first, or pass its path." >&2
	exit 1
fi

mkdir -p "$here/expected"
for map in "$here"/maps/*.omap
do
	[ -e "$map" ] || { echo "No maps in $here/maps/." >&2; exit 1; }
	name=$(basename "$map" .omap)
	# The field width counts bytes, so a name holding multibyte characters
	# pads short; the trailing space keeps the columns apart regardless.
	printf '%-50s ' "$name"
	"$tool" "$map" "$here/expected/$name.png" | sed 's/^.*: //'
done
