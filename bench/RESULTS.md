# Bench results

Recall of judged High and Medium findings on contests published after the agents' training
cutoff, traced through each stage of the run. A column counts the known bugs that reached at
least that stage, so the drop between two columns is what that pass lost.

Precision counts final findings that match a known bug or that the judge labels a plausible
new defect. Newest run first. Regenerate with `scripts/bench.sh table`.

| run | case | known H/M | considered | raw | merged | past scope | past PoC | final | final findings | precision | cost | hours |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
