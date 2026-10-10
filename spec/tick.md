# Tick

A mechanical watch timegrapher at `/tick`. The page listens to a watch through the
microphone, detects each beat, and plots the rate in seconds per day alongside the raw beats.
Everything runs in the browser; the server only serves the assets.

## Serving

### TICK-1
`GET /tick` serves the page. `GET /tick/` redirects (308) to `/tick`. The client scripts
and stylesheet are served under `/tick/static/`. No other route exists under `/tick`.

### TICK-2
The server keeps no state for tick: no database, no cookies, no config flag. Audio never
leaves the browser.

## Capture

### TICK-3
`start mic` asks for the microphone with echo cancellation, noise suppression, and automatic
gain control turned off, since each of them smears or drops the short beat transients. One
channel is read at the audio context's sample rate through an AudioWorklet (or a
ScriptProcessor where AudioWorklet is missing), in blocks of 2048 samples.

### TICK-4
The audio sample clock is the time reference: a beat's time is its sample position divided by
the sample rate. The simulator feeds whatever audio is due on a 50 ms timer, at most 1 second
of it per tick, so a backgrounded tab does not catch up in one burst. A sound card crystal is commonly off by 20-50 ppm (about 2-4 s/d), so a
`mic correction` setting in s/d is added to every rate reading.

### TICK-5
`simulate` feeds the detector a synthesized watch instead of the microphone: 28800 bph,
+6.0 s/d, 0.4 ms beat error, with background noise. It runs through the same detector and
displays as the microphone does. `stop` ends either source.

### TICK-6
A denied microphone, a missing `getUserMedia` (insecure context or old browser), or a failed
audio setup shows an error line and leaves the page idle.

## Detection

### TICK-7
Each sample goes through a 2 kHz biquad high-pass (ticks are light, high clicks; rumble,
voices, and knocks sit lower), then a rectified one-pole envelope (0.3 ms time constant). A
beat triggers on the upward crossing of an adaptive threshold; none count in the first 100 ms
while the filters settle. The threshold is the noise floor plus the larger of the
sensitivity multiple of the floor (TICK-21) or 25% of the median of the last 16 beat peaks over
it, and never below 1e-7 (only digital silence). The noise floor averages the envelope: with a
20 ms time constant while the filters settle, 0.3 s while under the threshold, 5 s while
briefly over it, and 20 ms again once it has stayed over for more than 60 ms (the room got
louder, or the stream started with silence). The beat peaks are forgotten after 3 seconds
without a beat and on every lock.

### TICK-23
Each reading reports `span`, the seconds between the first and last beat behind it, and
`settled`, true once `span` reaches the averaging window less two beat periods (older beats
are dropped, so a full window spans just under it). Until then the rate graph says how far
along it is:

| State | Rate graph |
|-------|------------|
| no beats heard | `listening for beats...` |
| beats heard, no beat rate | `ticks heard, finding the beat rate...` |
| beat rate known, no reading | `measuring: <span> of <window> s` and a progress bar |
| readings, window filling | the line dashed and dim, with the same text and bar at the top |
| window full | the line solid; no text |

Before the first reading, `span` counts the beats indexed since the lock. The readout's rate
is dimmed while its reading is not settled. Changing the averaging window, a relock, or a
silence that restarts the fit starts the window over.

### TICK-20
Once the beat rate is locked and at least 4 beats are in, an onset peaking over 4x the median
beat peak is a knock: it is not a beat, is counted as ignored, and the detector listens again
right away. Six in a row mean the watch itself got louder; the sixth is accepted and the beat
peaks start over from it.

### TICK-21
`sensitivity` sets how far over the noise floor a trigger must reach, as a multiple of it:
low 1.5, normal 0.8 (default), high 0.6, max 0.45. Plain white noise starts to trigger near
0.5. Changing it applies to a running source.

### TICK-22
While a source runs, an input meter in the readout shows the envelope level (peak since the
last frame, falling back 15% a frame) on a -100 to 0 dBFS scale with its value in dB, and
marks the noise floor and the trigger threshold. Under it a note says `No audio from the
microphone...` after 60 frames of exact zeros, or otherwise how many loud sounds were ignored.
Stopped, the meter reads `--`.

### TICK-8
After an onset, further crossings are ignored for 0.6 of the beat period once the rate is
known, or 50 ms before then, so the unlock, impulse, and drop sounds within one beat count
once.

Once the rate is known and a beat has been indexed, a crossing only triggers within 0.15 of a
period of a whole number of periods (at least one) after the last beat. A case or table that
rings on after a beat (common through a phone mic, and longer than 0.6 of a 100 ms period at
36000 bph) would otherwise trigger mid period, count as a beat, and lock out the real one. The
gate opens when no beat has come for 4 periods, and while the auto rate is in doubt
(TICK-10).

### TICK-9
Each beat is timed by constant fraction: where its envelope first reaches 30% of the way from
the noise floor to that beat's own peak (taken from the trigger on), interpolated between
samples. The search starts where this beat's rise began, stepping back from the trigger up to
12 ms (a late trigger may have skipped the unlock) but not past a dip under that level, so the
tail of the beat before is never picked. 30% is low enough that the unlock, often the weakest
sound, clears it every beat; a level near another sound's height would flip between them. Each
beat is reported with 2 ms of filtered signal before the trigger and 30 ms after, for the
scope.

### TICK-10
The beat rate is chosen from 12000, 14400, 18000, 19800, 21600, 25200, 28800, and 36000 bph.
In `auto`, each candidate is scored by folding the last 24 onset times by its period
(stretched by up to 0.4% either way, so a watch far off rate still lines up): the largest
share of onsets within 0.08 of a period of one phase. Missed beats don't lower it; a ringing
tail or stray click only dilutes it. A watch also folds cleanly at a multiple of its rate, so
the lower candidate wins unless a higher one scores over 0.05 more. It locks the best
candidate once at least 10 onsets are in and it scores 0.5 or more.

Once locked, the rate is in doubt when a full window of 24 onsets scores under 0.5 for it, or
when fewer than half the beats due got through the gate over the last 16 indexed beats (a
different watch mostly lands outside it). In doubt the gate is off and the onsets start over:
the locked rate stays if 10 fresh onsets fit it, and is replaced only once 24 fresh onsets fit
another. Choosing a rate in the select locks it immediately and never doubts it.

### TICK-11
Each beat gets an index: the previous index plus the interval divided by the period, rounded.
An onset that rounds to the same index as the previous one is dropped as noise. A gap of more
than 3 seconds restarts indexing at 0 and drops the beats before it from the fit.

## Readings

### TICK-12
Over the beats in the averaging window (4, 10, 30, or 60 seconds; default 10), the detector
fits `t = a + b*n + h*s` by least squares, where `n` is the beat index and `s` is +1 for even
and -1 for odd beats. With nominal period `P`:

- rate (s/d) = `86400 * (P / b - 1)` + mic correction; positive is fast
- beat error (ms) = `2 * |h| * 1000`

A reading needs at least 6 beats spanning at least 1 second, and is produced on every beat
after that. Beats that miss the fit by more than 0.5 ms and 5x the median miss (a beat caught
on its drop sound, a stray click) are left out and the fit is run once more, unless fewer than
6 beats would remain.

### TICK-13
Changing the beat rate, starting a source, or restarting the fit after a relock clears the
beat history and the graphs.

## Display

### TICK-14
The readout shows the current rate (s/d, signed, one decimal), beat error (ms, one decimal),
beat rate (bph, marked `auto` when detected), beats counted, and status: `idle`,
`listening` (no beat rate yet), `measuring` (beat rate known, averaging window still
filling, TICK-23), or `locked`. After `stop` the last reading and the graphs stay up.

### TICK-15
The rate graph plots readings against time over the visible span (30 s, 1 min, 5 min; default
1 min). The line grows from the left edge until it reaches the right, then scrolls with the
newest reading at the right edge. Readings more than 2 s apart are not joined. Zero is a fixed horizontal line in the middle; fast is
up and slow is down. The vertical range is symmetric, the smallest of 5, 10, 20, 30, 60, 120,
300, or 600 s/d that holds every visible reading with a 15% margin. The latest reading is
marked with a dot at the head of the line.

### TICK-16
Under the rate graph, sharing its time axis, the beat trace plots one dot per beat: x is the
beat's time and y is how early it landed against its nominal time (`t0 + n*P - t`, in ms,
where `t0` is the first indexed beat). The vertical range fits the visible dots with a 2 ms
minimum. Even and odd beats use two colors,
so a fast watch draws two rising lines, a slow one two falling lines, and the gap between them
is the beat error.

### TICK-17
Beside the beat trace, the scope overlays the last 16 beat waveforms aligned at their onsets
(-2 to +30 ms), newest brightest, colored by parity like the trace, and scaled to the largest
peak shown.

### TICK-18
The beat rate select, averaging window, visible span, sensitivity, and mic correction persist in
`localStorage` under `tick.settings`. Storage that throws or holds bad json falls back to the
defaults.

### TICK-19
Below 640px wide the scope stacks under the beat trace. The graphs redraw at device pixel
ratio on every animation frame while a source runs.
