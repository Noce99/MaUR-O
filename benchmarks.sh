#!/usr/bin/env bash
#
# Renders every benchmark map with the Rust map_to_image, compares the
# result against the stored reference image (rendered by the real
# OpenOrienteering Mapper, via the C++ tool that originally populated
# benchmarks/expected), and prints a pass/fail summary. Drives the whole
# 169-map suite in one pass instead of one test per map, since this is
# meant to be run often while iterating. See benchmarks/README.md.
#
# Usage: ./benchmarks.sh [--tolerance=N] [--threshold=F] [name-substring]

set -u

cd "$(dirname "$0")"
MAPS_DIR="benchmarks/maps"
EXPECTED_DIR="benchmarks/expected"
PREDICTIONS_DIR="benchmarks/predictions"

TOLERANCE=0
THRESHOLD=0
FILTER=""
for arg in "$@"; do
  case "$arg" in
    --tolerance=*) TOLERANCE="${arg#--tolerance=}" ;;
    --threshold=*) THRESHOLD="${arg#--threshold=}" ;;
    *) FILTER="$arg" ;;
  esac
done

echo "Building (release)..." >&2
cargo build --release --quiet 2>&1 | grep -v "unused_assignments\|maybe it is overwritten\|^\s*|\|note:" >&2
TOOL="target/release/map_to_image"
COMPARE="target/release/image_compare"

mkdir -p "$PREDICTIONS_DIR"

total=0
passed=0
failed_names=()
worst_name=""
worst_ratio="0"

for map in "$MAPS_DIR"/*.omap; do
  name="$(basename "$map" .omap)"
  if [[ -n "$FILTER" && "$name" != *"$FILTER"* ]]; then
    continue
  fi
  expected="$EXPECTED_DIR/$name.png"
  if [[ ! -f "$expected" ]]; then
    continue
  fi
  actual="$PREDICTIONS_DIR/$name.png"
  total=$((total + 1))

  render_output="$("$TOOL" "$map" "$actual" 2>&1)"
  if [[ $? -ne 0 ]]; then
    echo "FAIL  $name  (render error: $render_output)"
    failed_names+=("$name")
    continue
  fi

  compare_output="$("$COMPARE" "--tolerance=$TOLERANCE" "--threshold=$THRESHOLD" "$actual" "$expected" 2>&1)"
  compare_status=$?
  if [[ $compare_status -eq 0 ]]; then
    passed=$((passed + 1))
  else
    ratio="$(echo "$compare_output" | grep 'beyond tolerance' | sed -E 's/.*\(([0-9.]+)%\).*/\1/')"
    reason="$(echo "$compare_output" | head -1)"
    if [[ "$compare_status" -eq 3 ]]; then
      reason="size mismatch"
      ratio="100"
    fi
    echo "FAIL  $name  (${ratio:-?}% beyond tolerance; $reason)"
    failed_names+=("$name")
    if [[ -n "$ratio" ]] && (( $(echo "$ratio > $worst_ratio" | bc -l 2>/dev/null || echo 0) )); then
      worst_ratio="$ratio"
      worst_name="$name"
    fi
  fi
done

echo ""
echo "== $passed / $total passed (tolerance=$TOLERANCE, threshold=$THRESHOLD) =="
if [[ ${#failed_names[@]} -gt 0 ]]; then
  echo "Worst: $worst_name ($worst_ratio% beyond tolerance)"
  echo "Failed (${#failed_names[@]}): ${failed_names[*]}"
fi
