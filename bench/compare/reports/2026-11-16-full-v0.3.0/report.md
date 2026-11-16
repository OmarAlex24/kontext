# Comparison benchmark

## S1 · haiku-5-5

| arm | runs | score | cost USD (median [range]) | distill USD | session s | settle s | scripted turns | model requests | interventions | denials | errors |
|---|---|---|---|---|---|---|---|---|---|---|---|
| plain | 5 | 100.0 ± 0.0 % | 0.002 [0.002–0.002] | 0.000 [0.000–0.000] | 8.300 [6.500–8.500] | 0 [0–0] | 1 [1–1] | 4 [4–4] | 0 [0–0] | 0 [0–0] | 0 |
| kontext | 5 | 100.0 ± 0.0 % | 0.004 [0.004–0.004] | 0.001 [0.001–0.001] | 8.700 [8.200–10.5] | 2.700 [2.500–4.500] | 1 [1–1] | 5 [5–7] | 0 [0–0] | 0 [0–0] | 0 |
| gentle-ai | 5 | 100.0 ± 0.0 % | 0.010 [0.008–0.011] | 0.000 [0.000–0.000] | 16.1 [11.3–17.3] | 0 [0–0] | 1 [1–1] | 4 [4–6] | 0 [0–0] | 0 [0–0] | 0 |
| mem0 | 5 | 100.0 ± 0.0 % | 0.002 [0.002–0.002] | 0.000 [0.000–0.000] | 9.700 [7.900–11.7] | 15.1 [10.1–25.1] | 1 [1–1] | 4 [4–5] | 0 [0–0] | 0 [0–0] | 0 |
| beads | 5 | 100.0 ± 0.0 % | 0.003 [0.003–0.004] | 0.000 [0.000–0.000] | 9.800 [8.100–13.2] | 0 [0–0] | 1 [1–1] | 5 [4–6] | 0 [0–0] | 0 [0–1] | 0 |
| openspec | 5 | 100.0 ± 0.0 % | 0.002 [0.002–0.003] | 0.000 [0.000–0.000] | 8.200 [6.300–14.1] | 0 [0–0] | 1 [1–1] | 4 [4–8] | 0 [0–0] | 0 [0–1] | 0 |

Against plain (ANALYSIS.md rules):

- beads vs plain: cost +43%, time +18% (no claim)
- gentle-ai vs plain: cost +389%, time +94%
- kontext vs plain: cost +84%, time +5% (no claim)
- mem0 vs plain: cost +2% (no claim), time +17% (no claim)
- openspec vs plain: cost +6% (no claim), time -1% (no claim)

Against kontext (ANALYSIS.md rules):

- beads vs kontext: cost -22%, time +13% (no claim)
- gentle-ai vs kontext: cost +166%, time +85%
- mem0 vs kontext: cost -44%, time +11% (no claim)
- openspec vs kontext: cost -43%, time -6% (no claim)
- plain vs kontext: cost -46%, time -5% (no claim)

## S1 · sonnet-5-5

| arm | runs | score | cost USD (median [range]) | distill USD | session s | settle s | scripted turns | model requests | interventions | denials | errors |
|---|---|---|---|---|---|---|---|---|---|---|---|
| plain | 5 | 100.0 ± 0.0 % | 0.031 [0.026–0.035] | 0.000 [0.000–0.000] | 8.700 [7.900–13.2] | 0 [0–0] | 1 [1–1] | 4 [4–5] | 0 [0–0] | 0 [0–1] | 0 |
| kontext | 5 | 100.0 ± 0.0 % | 0.035 [0.033–0.039] | 0.001 [0.001–0.002] | 8.000 [7.200–8.700] | 4.100 [2.700–6.400] | 1 [1–1] | 5 [5–5] | 0 [0–0] | 0 [0–0] | 0 |
| gentle-ai | 5 | 100.0 ± 0.0 % | 0.160 [0.159–0.168] | 0.000 [0.000–0.000] | 12.4 [10.0–15.2] | 0 [0–0] | 1 [1–1] | 4 [4–5] | 0 [0–0] | 0 [0–0] | 0 |
| mem0 | 5 | 100.0 ± 0.0 % | 0.028 [0.028–0.031] | 0.000 [0.000–0.000] | 9.500 [8.100–11.2] | 10.1 [10.1–15.1] | 1 [1–1] | 4 [4–5] | 0 [0–0] | 0 [0–1] | 0 |
| beads | 5 | 100.0 ± 0.0 % | 0.037 [0.035–0.039] | 0.000 [0.000–0.000] | 8.900 [8.100–9.700] | 0 [0–0] | 1 [1–1] | 4 [4–4] | 0 [0–0] | 0 [0–0] | 0 |
| openspec | 5 | 100.0 ± 0.0 % | 0.029 [0.029–0.031] | 0.000 [0.000–0.000] | 8.300 [6.600–8.900] | 0 [0–0] | 1 [1–1] | 4 [4–5] | 0 [0–0] | 0 [0–1] | 0 |

Against plain (ANALYSIS.md rules):

- beads vs plain: cost +21%, time +2% (no claim)
- gentle-ai vs plain: cost +417%, time +43%
- kontext vs plain: cost +13% (no claim), time -8% (no claim)
- mem0 vs plain: cost -8% (no claim), time +9% (no claim)
- openspec vs plain: cost -6% (no claim), time -5% (no claim)

Against kontext (ANALYSIS.md rules):

- beads vs kontext: cost +7% (no claim), time +11% (no claim)
- gentle-ai vs kontext: cost +358%, time +55%
- mem0 vs kontext: cost -19% (no claim), time +19% (no claim)
- openspec vs kontext: cost -17% (no claim), time +4% (no claim)
- plain vs kontext: cost -11% (no claim), time +9% (no claim)

## S2 · haiku-5-5

| arm | runs | score | cost USD (median [range]) | distill USD | session s | settle s | scripted turns | model requests | interventions | denials | errors |
|---|---|---|---|---|---|---|---|---|---|---|---|
| plain | 5 | 100.0 ± 0.0 % | 0.003 [0.002–0.003] | 0.000 [0.000–0.000] | 11.3 [10.3–15.8] | 0 [0–0] | 1 [1–1] | 5 [4–5] | 0 [0–0] | 0 [0–0] | 0 |
| kontext | 5 | 100.0 ± 0.0 % | 0.005 [0.005–0.006] | 0.001 [0.001–0.002] | 12.5 [11.6–18.7] | 3.300 [2.800–7.500] | 1 [1–1] | 6 [6–8] | 0 [0–0] | 0 [0–0] | 0 |
| gentle-ai | 5 | 100.0 ± 0.0 % | 0.010 [0.009–0.013] | 0.000 [0.000–0.000] | 21.2 [17.5–33.4] | 0 [0–0] | 1 [1–1] | 7 [5–9] | 0 [0–0] | 0 [0–3] | 0 |
| mem0 | 5 | 100.0 ± 0.0 % | 0.003 [0.003–0.004] | 0.000 [0.000–0.000] | 19.3 [11.1–27.3] | 30.1 [15.1–45.1] | 1 [1–1] | 5 [5–7] | 0 [0–0] | 0 [0–0] | 0 |
| beads | 5 | 100.0 ± 0.0 % | 0.003 [0.003–0.004] | 0.000 [0.000–0.000] | 14.5 [12.5–14.8] | 0 [0–0] | 1 [1–1] | 5 [5–7] | 0 [0–0] | 0 [0–1] | 0 |
| openspec | 5 | 100.0 ± 0.0 % | 0.003 [0.003–0.003] | 0.000 [0.000–0.000] | 11.4 [10.8–13.3] | 0 [0–0] | 1 [1–1] | 5 [5–5] | 0 [0–0] | 0 [0–0] | 0 |

Against plain (ANALYSIS.md rules):

- beads vs plain: cost +31%, time +28%
- gentle-ai vs plain: cost +284%, time +88%
- kontext vs plain: cost +71%, time +11% (no claim)
- mem0 vs plain: cost +8% (no claim), time +71%
- openspec vs plain: cost +9% (no claim), time +1% (no claim)

Against kontext (ANALYSIS.md rules):

- beads vs kontext: cost -24%, time +16% (no claim)
- gentle-ai vs kontext: cost +124%, time +70%
- mem0 vs kontext: cost -37%, time +54%
- openspec vs kontext: cost -36%, time -9% (no claim)
- plain vs kontext: cost -42%, time -10% (no claim)

## S2 · sonnet-5-5

| arm | runs | score | cost USD (median [range]) | distill USD | session s | settle s | scripted turns | model requests | interventions | denials | errors |
|---|---|---|---|---|---|---|---|---|---|---|---|
| plain | 5 | 100.0 ± 0.0 % | 0.037 [0.035–0.039] | 0.000 [0.000–0.000] | 12.2 [10.0–15.2] | 0 [0–0] | 1 [1–1] | 6 [5–6] | 0 [0–0] | 1 [0–1] | 0 |
| kontext | 5 | 100.0 ± 0.0 % | 0.044 [0.040–0.048] | 0.002 [0.001–0.002] | 12.7 [9.500–16.1] | 5.300 [2.300–5.900] | 1 [1–1] | 6 [5–7] | 0 [0–0] | 0 [0–1] | 0 |
| gentle-ai | 5 | 100.0 ± 0.0 % | 0.145 [0.138–0.153] | 0.000 [0.000–0.000] | 18.2 [16.0–19.9] | 0 [0–0] | 1 [1–1] | 6 [5–7] | 0 [0–0] | 0 [0–1] | 0 |
| mem0 | 5 | 100.0 ± 0.0 % | 0.039 [0.038–0.043] | 0.000 [0.000–0.000] | 14.2 [11.5–17.6] | 10.1 [10.1–15.1] | 1 [1–1] | 6 [6–7] | 0 [0–0] | 1 [1–1] | 0 |
| beads | 5 | 100.0 ± 0.0 % | 0.050 [0.048–0.058] | 0.000 [0.000–0.000] | 13.8 [11.6–17.1] | 0 [0–0] | 1 [1–1] | 6 [5–7] | 0 [0–0] | 1 [0–1] | 0 |
| openspec | 5 | 100.0 ± 0.0 % | 0.040 [0.038–0.041] | 0.000 [0.000–0.000] | 12.4 [11.6–13.7] | 0 [0–0] | 1 [1–1] | 6 [5–6] | 0 [0–0] | 1 [0–1] | 0 |

Against plain (ANALYSIS.md rules):

- beads vs plain: cost +36%, time +13% (no claim)
- gentle-ai vs plain: cost +295%, time +49%
- kontext vs plain: cost +20%, time +4% (no claim)
- mem0 vs plain: cost +7% (no claim), time +16% (no claim)
- openspec vs plain: cost +9% (no claim), time +2% (no claim)

Against kontext (ANALYSIS.md rules):

- beads vs kontext: cost +13% (no claim), time +9% (no claim)
- gentle-ai vs kontext: cost +229%, time +43%
- mem0 vs kontext: cost -11% (no claim), time +12% (no claim)
- openspec vs kontext: cost -9% (no claim), time -2% (no claim)
- plain vs kontext: cost -17% (no claim), time -4% (no claim)

## L · haiku-5-5

| arm | runs | score | cost USD (median [range]) | distill USD | session s | settle s | scripted turns | model requests | interventions | denials | errors |
|---|---|---|---|---|---|---|---|---|---|---|---|
| plain | 5 | 3.8 ± 3.0 / 14 | 0.121 [0.114–0.133] | 0.000 [0.000–0.000] | 352.6 [338.8–383.6] | 0 [0–0] | 19 [19–19] | 92 [83–100] | 1 [0–1] | 9 [8–13] | 0 |
| kontext | 5 | 12.6 ± 0.9 / 14 | 0.176 [0.165–0.188] | 0.034 [0.033–0.035] | 455.0 [397.0–470.1] | 18.4 [15.3–22.3] | 19 [19–19] | 108 [99–124] | 1 [0–2] | 4 [3–7] | 0 |
| gentle-ai | 5 | 6.6 ± 0.9 / 14 | 0.195 [0.173–0.212] | 0.000 [0.000–0.000] | 400.4 [351.4–413.9] | 0 [0–0] | 19 [19–19] | 89 [75–101] | 1 [0–1] | 3 [2–5] | 0 |
| mem0 | 5 | 6.8 ± 2.6 / 14 | 0.120 [0.088–0.122] | 0.000 [0.000–0.000] | 365.9 [345.6–387.3] | 260.4 [250.5–315.4] | 19 [19–19] | 86 [84–95] | 1 [0–2] | 5 [2–7] | 0 |
| beads | 5 | 6.0 ± 3.9 / 14 | 0.137 [0.128–0.172] | 0.000 [0.000–0.000] | 454.8 [348.2–1079.5] | 0 [0–0] | 19 [19–19] | 104 [85–119] | 1 [0–1] | 7 [4–12] | 0 |
| openspec | 5 | 5.2 ± 2.8 / 14 | 0.125 [0.121–0.131] | 0.000 [0.000–0.000] | 355.3 [320.8–387.1] | 0 [0–0] | 19 [19–19] | 94 [84–96] | 1 [0–3] | 6 [6–13] | 0 |

### L by class, by day, confusions

| arm | class A | class B | class answer | class C | class discarded | day 3 (new chat) | day 4 (machine B) | confusions | coded before loss | regressed |
|---|---|---|---|---|---|---|---|---|---|---|
| plain | 1.0 / 3 | 0.4 / 6 | 1.0 / 2 | 1.0 / 2 | 0.4 / 1 | 3.0 / 7 | 0.8 / 7 | 1 [1–1] | none | none |
| kontext | 3.0 / 3 | 5.0 / 6 | 1.8 / 2 | 1.8 / 2 | 1.0 / 1 | 6.6 / 7 | 6.0 / 7 | 0 [0–0] | R1, R11, R13, R14, R3, R4, R5, R6, R7 | none |
| gentle-ai | 0.6 / 3 | 2.2 / 6 | 1.6 / 2 | 1.2 / 2 | 1.0 / 1 | 5.8 / 7 | 0.8 / 7 | 1 [0–1] | none | none |
| mem0 | 1.6 / 3 | 1.8 / 6 | 1.8 / 2 | 1.2 / 2 | 0.4 / 1 | 3.6 / 7 | 3.2 / 7 | 1 [1–2] | none | none |
| beads | 1.4 / 3 | 1.6 / 6 | 1.6 / 2 | 0.8 / 2 | 0.6 / 1 | 3.4 / 7 | 2.6 / 7 | 1 [0–1] | R1, R11, R13, R14, R5, R7 | none |
| openspec | 1.4 / 3 | 1.2 / 6 | 1.2 / 2 | 1.0 / 2 | 0.4 / 1 | 4.0 / 7 | 1.2 / 7 | 1 [1–2] | R13, R3, R6 | none |

Failed requirements (runs): beads: R1×4, R10×4, R11×2, R12×3, R2×5, R3×2, R4×4, R5×4, R6×2, R7×4, R8×2, R9×4; gentle-ai: R1×5, R11×5, R12×4, R13×2, R14×2, R2×5, R4×5, R5×5, R7×4; kontext: R2×5, R3×1, R6×1; mem0: R1×2, R10×4, R11×1, R12×5, R13×2, R14×1, R2×5, R4×2, R5×3, R6×2, R7×2, R8×3, R9×4; openspec: R1×4, R10×3, R11×4, R12×5, R14×3, R2×5, R3×1, R4×4, R5×4, R6×2, R7×3, R8×3, R9×3; plain: R1×4, R10×5, R11×5, R12×5, R13×2, R14×4, R2×5, R3×1, R4×4, R5×4, R6×1, R7×4, R8×3, R9×4

Against plain (ANALYSIS.md rules):

- beads vs plain: cost +13% (no claim), time +29%, requirements no claim (+2.2 requirements, p = 0.397)
- gentle-ai vs plain: cost +61%, time +14% (no claim), requirements no claim (+2.8 requirements, p = 0.119)
- kontext vs plain: cost +46%, time +29%, requirements differs (+8.8 requirements, p = 0.008)
- mem0 vs plain: cost -1% (no claim), time +4% (no claim), requirements no claim (+3.0 requirements, p = 0.151)
- openspec vs plain: cost +3% (no claim), time +1% (no claim), requirements no claim (+1.4 requirements, p = 0.540)

Against kontext (ANALYSIS.md rules):

- beads vs kontext: cost -22%, time -0% (no claim), requirements differs (-6.6 requirements, p = 0.016)
- gentle-ai vs kontext: cost +10% (no claim), time -12% (no claim), requirements differs (-6.0 requirements, p = 0.008)
- mem0 vs kontext: cost -32%, time -20% (no claim), requirements differs (-5.8 requirements, p = 0.016)
- openspec vs kontext: cost -29%, time -22%, requirements differs (-7.4 requirements, p = 0.008)
- plain vs kontext: cost -31%, time -23%, requirements differs (-8.8 requirements, p = 0.008)

## L · sonnet-5-5

| arm | runs | score | cost USD (median [range]) | distill USD | session s | settle s | scripted turns | model requests | interventions | denials | errors |
|---|---|---|---|---|---|---|---|---|---|---|---|
| plain | 5 | 0.2 ± 0.4 / 14 | 0.873 [0.797–0.910] | 0.000 [0.000–0.000] | 296.9 [283.6–340.5] | 0 [0–0] | 19 [19–19] | 66 [64–70] | 3 [2–4] | 4 [3–4] | 0 |
| kontext | 5 | 12.8 ± 0.4 / 14 | 0.977 [0.934–1.165] | 0.032 [0.030–0.033] | 317.7 [290.9–382.8] | 21.6 [18.9–23.4] | 19 [19–19] | 87 [78–94] | 0 [0–0] | 2 [0–6] | 0 |
| gentle-ai | 5 | 6.4 ± 1.3 / 14 | 1.611 [1.470–1.701] | 0.000 [0.000–0.000] | 347.1 [326.6–370.2] | 0 [0–0] | 19 [19–19] | 70 [62–77] | 2 [0–3] | 1 [0–2] | 0 |
| mem0 | 5 | 2.6 ± 1.1 / 14 | 0.833 [0.799–0.887] | 0.000 [0.000–0.000] | 297.9 [285.5–328.5] | 260.5 [205.4–395.6] | 19 [19–19] | 64 [61–67] | 2 [1–3] | 2 [1–4] | 0 |
| beads | 5 | 1.4 ± 2.6 / 14 | 0.940 [0.866–1.006] | 0.000 [0.000–0.000] | 321.6 [292.6–327.8] | 0 [0–0] | 19 [19–19] | 64 [59–67] | 3 [1–3] | 2 [1–4] | 0 |
| openspec | 5 | 0.2 ± 0.4 / 14 | 0.900 [0.728–0.920] | 0.000 [0.000–0.000] | 306.0 [258.3–317.2] | 0 [0–0] | 19 [19–19] | 69 [58–71] | 2 [1–3] | 4 [1–5] | 0 |

### L by class, by day, confusions

| arm | class A | class B | class answer | class C | class discarded | day 3 (new chat) | day 4 (machine B) | confusions | coded before loss | regressed |
|---|---|---|---|---|---|---|---|---|---|---|
| plain | 0.0 / 3 | 0.0 / 6 | 0.2 / 2 | 0.0 / 2 | 0.0 / 1 | 0.2 / 7 | 0.0 / 7 | 2 [2–3] | none | none |
| kontext | 2.2 / 3 | 5.6 / 6 | 2.0 / 2 | 2.0 / 2 | 1.0 / 1 | 6.2 / 7 | 6.6 / 7 | 0 [0–0] | none | none |
| gentle-ai | 0.8 / 3 | 2.0 / 6 | 1.0 / 2 | 1.8 / 2 | 0.8 / 1 | 6.4 / 7 | 0.0 / 7 | 1 [1–2] | none | none |
| mem0 | 1.0 / 3 | 0.4 / 6 | 1.0 / 2 | 0.2 / 2 | 0.0 / 1 | 1.4 / 7 | 1.2 / 7 | 3 [2–3] | none | none |
| beads | 0.0 / 3 | 0.4 / 6 | 0.6 / 2 | 0.2 / 2 | 0.2 / 1 | 0.8 / 7 | 0.6 / 7 | 2 [0–3] | none | none |
| openspec | 0.0 / 3 | 0.0 / 6 | 0.2 / 2 | 0.0 / 2 | 0.0 / 1 | 0.2 / 7 | 0.0 / 7 | 3 [2–3] | none | none |

Failed requirements (runs): beads: R1×5, R10×5, R11×4, R12×4, R13×5, R14×4, R2×5, R3×3, R4×5, R5×5, R6×4, R7×5, R8×4, R9×5; gentle-ai: R1×5, R11×5, R12×5, R13×1, R14×5, R2×5, R4×5, R5×5, R7×1, R8×1; kontext: R13×4, R2×2; mem0: R1×2, R10×4, R11×5, R12×5, R13×4, R14×3, R2×5, R3×2, R4×5, R5×4, R6×5, R7×4, R8×5, R9×4; openspec: R1×5, R10×5, R11×5, R12×5, R13×5, R14×5, R2×5, R3×4, R4×5, R5×5, R6×5, R7×5, R8×5, R9×5; plain: R1×5, R10×5, R11×5, R12×5, R13×5, R14×5, R2×5, R3×4, R4×5, R5×5, R6×5, R7×5, R8×5, R9×5

Against plain (ANALYSIS.md rules):

- beads vs plain: cost +8% (no claim), time +8% (no claim), requirements no claim (+1.2 requirements, p = 0.722)
- gentle-ai vs plain: cost +84%, time +17% (no claim), requirements differs (+6.2 requirements, p = 0.008)
- kontext vs plain: cost +12% (no claim), time +7% (no claim), requirements differs (+12.6 requirements, p = 0.008)
- mem0 vs plain: cost -5% (no claim), time +0% (no claim), requirements differs (+2.4 requirements, p = 0.016)
- openspec vs plain: cost +3% (no claim), time +3% (no claim), requirements no claim (+0.0 requirements, p = 1.000)

Against kontext (ANALYSIS.md rules):

- beads vs kontext: cost -4% (no claim), time +1% (no claim), requirements differs (-11.4 requirements, p = 0.008)
- gentle-ai vs kontext: cost +65%, time +9% (no claim), requirements differs (-6.4 requirements, p = 0.008)
- mem0 vs kontext: cost -15% (no claim), time -6% (no claim), requirements differs (-10.2 requirements, p = 0.008)
- openspec vs kontext: cost -8% (no claim), time -4% (no claim), requirements differs (-12.6 requirements, p = 0.008)
- plain vs kontext: cost -11% (no claim), time -7% (no claim), requirements differs (-12.6 requirements, p = 0.008)

