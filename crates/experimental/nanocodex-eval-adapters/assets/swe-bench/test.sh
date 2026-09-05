#!/bin/bash
set -euo pipefail

if [[ -f /opt/miniconda3/etc/profile.d/conda.sh ]]; then
  source /opt/miniconda3/etc/profile.d/conda.sh
  conda activate testbed
elif [[ -f /root/miniconda3/etc/profile.d/conda.sh ]]; then
  source /root/miniconda3/etc/profile.d/conda.sh
  conda activate testbed
fi

python /tests/grade.py
