#!/bin/bash
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
# Populates benchmarks/maps/ from a collection of maps, then renders the
# reference images into benchmarks/expected/.
#
# Maps are searched recursively, and copied -- the collection is left
# untouched. They are renamed to a two digit ordinal, an underscore, and the
# original name with spaces replaced by underscores, which keeps the test names
# stable and free of quoting problems. The ordinal follows the alphabetical
# order of those names, so the same collection always yields the same numbering.
#
# Usage: benchmarks/setup.sh [--force] <renderer> <map-collection>
#
#   <renderer>         a map_to_image executable, used to render the references
#   <map-collection>   a directory to search recursively for .omap files
#   --force            replace an existing benchmarks/maps/ and expected/

set -euo pipefail

usage()
{
	cat <<-END
	Usage: $0 [--force] <renderer> <map-collection>

	Copies every .omap found below <map-collection> into benchmarks/maps/,
	renaming it to a two digit ordinal and the original name with spaces
	replaced by underscores, then renders the reference images with
	<renderer> into benchmarks/expected/.

	  <renderer>         a map_to_image executable
	  <map-collection>   a directory, searched recursively
	  --force            replace existing benchmarks
	END
}

force=no
args=()
for argument in "$@"
do
	case $argument in
	--force|-f) force=yes ;;
	-h|--help)  usage ; exit 0 ;;
	-*)         echo "$0: unknown option '$argument'" >&2 ; exit 1 ;;
	*)          args+=("$argument") ;;
	esac
done

if [ ${#args[@]} -ne 2 ]
then
	usage >&2
	exit 1
fi

renderer=${args[0]}
collection=${args[1]}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
maps="$here/maps"
expected="$here/expected"

if [ ! -x "$renderer" ]
then
	echo "$0: '$renderer' is not an executable." >&2
	exit 1
fi
if [ ! -d "$collection" ]
then
	echo "$0: '$collection' is not a directory." >&2
	exit 1
fi

# Refuse to throw away an existing set of benchmarks without being told to:
# regenerating them costs minutes, and the numbering may well come out
# different if the collection has changed in the meantime.
if [ "$force" = no ]
then
	for directory in "$maps" "$expected"
	do
		if [ -d "$directory" ] && [ -n "$(ls -A -- "$directory" 2>/dev/null)" ]
		then
			echo "$0: '$directory' is not empty. Pass --force to replace it." >&2
			exit 1
		fi
	done
fi

# Collect the maps as <sanitized name><tab><path> so that they can be sorted by
# the name they will get, not by the directory they happen to live in.
entries=()
while IFS= read -r -d '' file
do
	name=$(basename -- "$file")
	name=${name%.[oO][mM][aA][pP]}
	entries+=("${name// /_}"$'\t'"$file")
done < <(find "$collection" -type f -iname '*.omap' -print0)

if [ ${#entries[@]} -eq 0 ]
then
	echo "$0: no .omap files found under '$collection'." >&2
	exit 1
fi

rm -rf -- "$maps" "$expected"
mkdir -p -- "$maps"

count=0
while IFS=$'\t' read -r -d '' name file
do
	count=$((count + 1))
	printf -v ordinal '%02d' "$count"
	cp -- "$file" "$maps/${ordinal}_${name}.omap"
done < <(printf '%s\0' "${entries[@]}" | LC_ALL=C sort -z)

echo "Collected $count maps into ${maps#"$here"/}/, rendering references:"
echo
"$here/regenerate.sh" "$renderer"
echo
echo "Re-run CMake so the new benchmarks are picked up, for example:"
echo "    cmake -S . -B build"
