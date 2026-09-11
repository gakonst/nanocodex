"""Offline plot: uv run --no-project --with matplotlib python <script> summary.json."""
import json
import sys
from pathlib import Path
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
from matplotlib.lines import Line2D
from matplotlib.ticker import FuncFormatter

source = Path(sys.argv[1])
data = json.loads(source.read_text())
models = ['gpt-5.6-luna', 'gpt-5.6-terra', 'gpt-5.6-sol', 'gpt-6-astra']
paths = ['responses_http', 'responses_ws', 'nanocodex_node', 'openai_agents']
labels = ['Responses HTTP', 'Responses WS (reused)', 'Nanocodex Node (fresh)', 'OpenAI Agents (fresh)']
colors = ['#2878b5', '#249c88', '#d29421', '#c84b63']
fig, axes = plt.subplots(2, 2, figsize=(12, 7.8), sharex=True, sharey=True)
for row, metric in enumerate(['ttft', 'completion']):
    for col, tier in enumerate(['default', 'fast']):
        ax = axes[row, col]
        for index, path in enumerate(paths):
            for m, model in enumerate(models):
                record = next(g for g in data['groups'] if (g['model'], g['tier'], g['path']) == (model, tier, path))
                value = record[metric + '_median_ms']
                bounds = record[metric + '_range_ms']
                if value is None or bounds is None:
                    continue
                y = 3 - m + (1.5-index)*.13
                ax.errorbar(value/1000, y, xerr=[[(value-bounds[0])/1000], [(bounds[1]-value)/1000]],
                            fmt='o', color=colors[index], capsize=2.5, markersize=5, linewidth=1.2)
        ax.set_xscale('log')
        ax.set_xlim(.3, 150)
        ax.set_xticks([.5, 1, 2, 5, 10, 20, 50, 100])
        ax.xaxis.set_major_formatter(FuncFormatter(lambda x, _: f'{x:g}'))
        ax.set_yticks([3, 2, 1, 0], ['Luna', 'Terra', 'Sol', 'Astra'])
        ax.grid(axis='x', alpha=.2)
        ax.set_axisbelow(True)
        for side in ['top', 'right']:
            ax.spines[side].set_visible(False)
        ax.set_title(('Standard' if tier == 'default' else 'Fast requested') + ' · ' + ('first visible text' if metric == 'ttft' else 'turn completion'), loc='left', fontsize=11)
        if row == 1:
            ax.set_xlabel('Seconds (log scale)')
fig.suptitle('Same arithmetic prompt · low reasoning · 3 trials per configuration', fontsize=15, y=.99)
fig.legend([Line2D([0],[0],marker='o',color=c,linewidth=1.2) for c in colors], labels,
           loc='upper center', bbox_to_anchor=(.5,.956), ncol=4, frameon=False, fontsize=9)
fig.text(.065,.035,'Dots = medians; whiskers = observed min–max, not confidence intervals. No p95 estimate from n=3.\nWS timings exclude connection setup; fresh Node turn timings include connection establishment. Node is not the deployed managed service.',fontsize=9,color='#444444')
fig.tight_layout(rect=[.02,.105,.99,.91])
fig.savefig(source.parent/'latency.svg',bbox_inches='tight')
svg = source.parent/'latency.svg'
svg.write_text('\n'.join(line.rstrip() for line in svg.read_text().splitlines()) + '\n')
fig.savefig(source.parent/'latency.png',dpi=160,bbox_inches='tight')
