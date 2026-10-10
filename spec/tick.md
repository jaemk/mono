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

### TICK-24
`record 30 s` keeps the next 30 seconds of input, exactly the samples the detector gets
(unprocessed, at the input's sample rate), from the running source, or from the mic, opened
first, when idle. The button counts down while recording, and the live display keeps running.
When it finishes, the source stops, the recording is analyzed (TICK-25), and a `save
recording (<secs> s, <size> MB)` link (TICK-35) offers it as a mono 32-bit float WAV named
`tick-<yyyymmdd>-<hhmmss>-<beat rate setting>.wav` (clicking it downloads the file; browsers
block a download not started by a click). The link stays until the next recording replaces it.
`stop` or a new source cancels a recording in progress without saving or analyzing it.
Recordings never leave the browser except as that download.

### TICK-25
`analyze file` reads an audio file the browser can decode (decoded at 48 kHz, channels mixed
to mono, at most the first 5 minutes) and analyzes it as a finished recording is:

- the whole recording runs through a detector with the current settings, and the graphs show
  its readings and beats over its own length, labeled in seconds from its start
- the readout shows one fit over the longest unbroken run of beats in it (the rate label says
  `rate over <length> of <name>`), and the status reads `analyzed`
- without enough steady beats for a fit, the rate graph says `no steady beats found in <name>`
- changing the beat rate, averaging, sensitivity, or mic correction analyzes the same audio
  again; a live source or simulate replaces the analysis

A file that can't be decoded shows an error line. A decode that finishes after another source
started is dropped.

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
right away. Only onsets taken as beats (not ones dropped as noise, TICK-11) count toward the
beat peaks, so quieter clicks between ticks can't pull the median down until the ticks look
like knocks. Six loud onsets within 2 seconds mean the watch itself is louder than the beat
peaks say (moved closer, or the peaks had filled with quieter clicks); the sixth is accepted
and the beat peaks start over from it.

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
once. Once there is a reading, an onset more than 0.25 of a period from where the fit expects a
beat only gets the 50 ms, so a stray click can't hold off the beat after it.

### TICK-9
Each beat is timed by constant fraction: where its envelope first reaches halfway from the
noise floor to that beat's own peak, interpolated between samples, searched from 12 ms before
the trigger to 30 ms after it. This does not move with how loud the beat is, and a late trigger
(on the impulse or drop sound instead of the unlock) is timed the same as an early one. Each
beat is reported with 2 ms of filtered signal before the trigger and 30 ms after, for the
scope.

### TICK-10
The beat rate is chosen from 12000, 14400, 18000, 19800, 21600, 25200, 28800, and 36000 bph.
In `auto`, the detector scores each candidate against the last 24 onset intervals: the
distance from each interval to its nearest whole number of periods, as a fraction of the
period, plus 0.02 for each interval that spans missed beats, averaged over the best fitting
3/4 of the intervals so a few noise onsets don't sink the true rate. It locks the best candidate once
one scores 0.08 or less (after at least 8 intervals). When a full window of 24 no longer fits
the locked rate, the window is dropped and the choice is made again on at least 8 fresh
intervals: the locked rate stays if it fits them, otherwise the best fitting candidate
replaces it. Choosing a rate in the select locks it immediately.

### TICK-11
Each beat gets an index: once there is a reading, the index the latest fit puts it nearest;
before then, the previous index plus the interval divided by the period, rounded. An onset
behind the previous index is dropped. One on the same index replaces the previous beat if it
lands nearer where the fit expects that beat (and is reported with `replaced`), and is
otherwise dropped as noise. A gap of more
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
on its drop sound, a stray click) are left out and the fit is run again, until the beats left
out stop changing (at most 3 refits), unless fewer than 6 beats would remain. A fit more than
1500 s/d off nominal has indexed noise as beats: it gives no reading, and the fit and indexing
start over from the next beat.

### TICK-13
Changing the beat rate, starting a source, or restarting the fit after a relock clears the
beat history and the graphs.

## Display

### TICK-14
The readout shows the current rate (s/d, signed, one decimal), beat error (ms, one decimal),
beat rate (bph, marked `auto` when detected), beats counted, and status: `idle`,
`listening` (no beats heard, or no beat rate yet), `measuring` (beat rate known, averaging window still
filling, TICK-23), `locked`, or `analyzed` (a recording or file on show, TICK-25). After `stop` the last reading and the graphs stay up.
The status is the page's only live region, rewritten only when the state changes, so a screen
reader announces state changes and not every beat's reading.

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
The beat rate select, averaging window, visible span, sensitivity, mic correction, and target
(TICK-30) persist in `localStorage` under `tick.settings`. Storage that throws or holds bad json falls back to the
defaults.

### TICK-26
Each graph (rate, beat trace, beat scope), each readout value (rate, beat error, beat rate,
beats, input), and the status has a small `i` button. It opens a modal dialog explaining what
is shown and how to read it, with typical values where they help. The dialog closes with its
close button, a click on the backdrop, or escape, and falls back to an open attribute where
the browser has no modal dialog.

### TICK-19
Below 640px wide the readout comes first and the scope and positions stack under the beat
trace. The graphs redraw at device pixel ratio on every animation frame while a source runs.

## Working aids

### TICK-27
While a source runs, the page holds a screen wake lock where the browser offers one, so a
phone's screen stays on through a measurement. `stop` releases it. When the browser drops it
(the tab was hidden), it is taken again once the tab shows while the source still runs. A lock
granted after the source stopped is released at once; a refused one is ignored.

### TICK-28
`big readout` (or `f`) covers the page with the rate, beat error, and status in type large
enough to read at arm's length, fullscreen where the browser allows it. Next to the rate, while
a source runs, an arrow shows the trend against the reading 5 s or more before the latest: up
when the rate rose by more than 1 s/d, down when it fell by more, and level otherwise. `exit`,
escape, `f`, or leaving fullscreen closes it. The rate there is dimmed and colored as in the
readout.

### TICK-29
The positions panel keeps a result per watch position (dial up, dial down, crown up, down,
left, right). `save result` stores the rate, beat error, and beat rate under the chosen
position: an analyzed recording's fit, or the latest live reading once settled (TICK-23);
with neither it is disabled. Saving a position again replaces it, and the position select
then moves to the next one not yet saved. Rows show in position order, each with a `remove`
button, under the average rate and beat error and the delta (fastest less slowest rate).
`copy` writes the table as text to the clipboard and says whether it worked; `clear` empties
it. The rows persist in `localStorage` under `tick.session`; malformed rows and repeats of a
position are dropped on load.

### TICK-30
The `target` setting picks a rate band: off (default), -4 to +6 s/d, or +-5, 10, 20, or 30
s/d. With one set, the rate graph shades the band with dashed edges (its range grows to hold
the band), and a rate outside it shows amber in the readout, the big readout, and the
positions table. Beat error shows amber over 1 ms and red over 3 ms there too.

### TICK-31
Pointing at the rate graph or the beat trace (hover, or a tap on touch) marks the reading or
beat nearest that time on both, with a line, a dot, and its value and time (`at -12.3s` live,
`at 12.3s` into a recording). The label sits on the side with more room. A mouse leaving the
graph clears it; a finger lifting leaves it until the next tap. A new source starts without
one.

### TICK-32
An audio file dragged over the page shows a `drop an audio file to analyze it` overlay, and
dropping it analyzes it as `analyze file` does (TICK-25). Drags that carry no files are left
to the browser.

### TICK-33
The settings sit in a section that, below 640px wide, folds under a `settings` toggle and
starts folded. Growing past 640px opens it. The buttons are grouped as `live` (start mic,
simulate, stop, big readout) and `offline` (record, analyze file, save).

### TICK-34
Keys: space starts the mic, or stops the running source, in place of pressing the focused
button; `r` records 30 s (TICK-24); `f` toggles the big readout (TICK-28); escape closes the
info dialog or the big readout. Keys are ignored with ctrl, meta, or alt held, while typing in
a setting, and while an info dialog is open.

### TICK-35
While recording, the record button fills from the left as the 30 s go by. The save link names
the recording's length and size, and its tooltip names the file it downloads.
