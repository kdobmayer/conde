#!/bin/sh
# Rebuild the estate drawing: SVG, PNG at 2x, PDF, from the scripts and the icons kept here.
set -eu
cd "$(dirname "$0")"
export DIAGRAM_ICONS="$PWD/icons"
python3 build_estate.py
python3 build_estate_option_a.py
rsvg-convert -z 2 -o conde-cloud-estate.png conde-cloud-estate.svg
rsvg-convert -f pdf -o conde-cloud-estate.pdf conde-cloud-estate.svg
rm -rf __pycache__
