"""Original 'Twilight' sound pack for midna, synthesized from scratch (no samples).
See README.md for which rendered file ships as which sound, and why the KEEP sounds aren't rebuilt.

Building blocks: glottal pulse + moving formants (Midna-style gibberish chirps), FM glass
bells, reversed swells, ring-mod shimmer, a small feedback-delay reverb. Pure stdlib.
Run: python3 synth.py  -> out/*.wav (44.1 kHz, 16-bit mono)
"""
import math, random, struct, wave, os

SR = 44100
random.seed(7)
OUT = os.path.join(os.path.dirname(__file__), "out")
os.makedirs(OUT, exist_ok=True)


def n(sec):
    return int(sec * SR)


def silence(sec):
    return [0.0] * n(sec)


def mix(*parts, at=None):
    """Mix lists; `at` = start offsets in seconds."""
    at = at or [0] * len(parts)
    length = max(n(o) + len(p) for p, o in zip(parts, at))
    out = [0.0] * length
    for p, o in zip(parts, at):
        p = fade(p, 0.002, 0.03)  # no clicks where a voice starts or is cut off
        s = n(o)
        for i, v in enumerate(p):
            out[s + i] += v
    return out


def gain(x, g):
    return [v * g for v in x]


def env_adsr(length, a, d, s, r):
    na, nd, nr = n(a), n(d), n(r)
    ns = max(0, length - na - nd - nr)
    e = []
    e += [i / max(1, na) for i in range(na)]
    e += [1 - (1 - s) * i / max(1, nd) for i in range(nd)]
    e += [s] * ns
    e += [s * (1 - i / max(1, nr)) for i in range(nr)]
    return (e + [0.0] * length)[:length]


def env_exp(length, tau):
    return [math.exp(-i / (tau * SR)) for i in range(length)]


def apply(x, e):
    return [a * b for a, b in zip(x, e)]



def tone(freq, dur, harmonics=(1, 0.3, 0.1), a=0.006, tau=0.3, detune=0.0):
    """Soft additive tone: whole-number harmonics only, so it's always in tune. No FM clang."""
    out = []
    for i in range(n(dur)):
        t = i / SR
        v = sum(h * math.sin(2 * math.pi * freq * (k + 1) * t * (1 + detune * k)) for k, h in enumerate(harmonics))
        att = min(1, t / a) if a else 1
        out.append(v * att * math.exp(-t / tau))
    return fade(out, 0.0, min(0.03, dur / 3))


# v1 sounds the user liked: never re-rendered (their noise/randomness would change).
KEEP = {"tw-ping", "tw-denied", "tw-sent", "tw-closed", "tw-copy", "tw-failed", "tw-tick", "tw-attention", "tw-done", "tw-portal-a", "tw-approved-c", "tw-portal-c", "tw-sparkle-b"}


def seed(name):
    random.seed(sum(map(ord, name)))


# ------------------------------------------------------------------ voices

def bell(freq, dur, ratio=3.5, index=2.2, tau=0.35, bright=1.0):
    """FM glass bell: inharmonic modulator, index decays faster than the amplitude."""
    out = []
    for i in range(n(dur)):
        t = i / SR
        idx = index * bright * math.exp(-t / (tau * 0.4))
        m = math.sin(2 * math.pi * freq * ratio * t)
        out.append(math.sin(2 * math.pi * freq * t + idx * m) * math.exp(-t / tau))
    return fade(out, 0.002, min(0.03, dur / 3))


def shimmer(x, freq=1310, depth=0.35):
    """Ring-mod a little of the signal against itself: the 'twilight' glassiness."""
    return [v * (1 - depth + depth * math.sin(2 * math.pi * freq * i / SR)) for i, v in enumerate(x)]


class Biquad:
    def __init__(self):
        self.x1 = self.x2 = self.y1 = self.y2 = 0.0

    def bandpass(self, x, f, q):
        w = 2 * math.pi * f / SR
        alpha = math.sin(w) / (2 * q)
        b0, b2 = alpha, -alpha
        a0, a1, a2 = 1 + alpha, -2 * math.cos(w), 1 - alpha
        y = (b0 * x + b2 * self.x2 - a1 * self.y1 - a2 * self.y2) / a0
        self.x2, self.x1, self.y2, self.y1 = self.x1, x, self.y1, y
        return y


VOWELS = {  # (F1, F2, F3) pushed up for a small, bright voice
    "a": (950, 1500, 3100), "e": (600, 2500, 3400), "i": (380, 2900, 3800),
    "o": (620, 1100, 3000), "u": (420, 1000, 2900), "eh": (750, 2000, 3200),
}


def gibber(syllables, base=470, rate=0.075, glide=0.0, breath=0.12, vib=7.0):
    """Midna-style gibberish: a glottal pulse train with a pitch contour, through three
    formants that move between vowel targets each syllable. `syllables` = [(vowel, pitch_mult, len_mult)]."""
    out = []
    filt = [Biquad(), Biquad(), Biquad()]
    phase = 0.0
    total = sum(n(rate * lm) for _, _, lm in syllables)
    k = 0
    prev = VOWELS[syllables[0][0]]
    for vi, (v, pm, lm) in enumerate(syllables):
        L = n(rate * lm)
        tgt = VOWELS[v]
        for i in range(L):
            u = i / L
            frac = k / total
            f0 = base * pm * (1 + glide * frac) * (1 + 0.02 * math.sin(2 * math.pi * vib * k / SR))
            phase += f0 / SR
            if phase >= 1:
                phase -= 1
            # Rosenberg-ish glottal pulse + breath noise
            g = (math.sin(math.pi * phase / 0.6) ** 2 if phase < 0.6 else 0.0) - 0.3
            src = g + breath * (random.random() * 2 - 1)
            blend = min(1, u * 3)  # formants slide into the vowel
            fs = [p + (q - p) * blend for p, q in zip(prev, tgt)]
            y = sum(w * f.bandpass(src, fc, qq) for f, fc, qq, w in zip(filt, fs, (6, 9, 11), (1.0, 0.7, 0.35)))
            # each syllable gets its own little amplitude bump
            out.append(y * (math.sin(math.pi * u) ** 0.6))
            k += 1
        prev = tgt
    return out


def noise_sweep(dur, f0, f1, q=3, tau=None, path=None):
    """Band-passed noise whose centre moves from f0 to f1 (or along `path`: u in 0..1 -> Hz)."""
    b = Biquad()
    out = []
    L = n(dur)
    for i in range(L):
        f = path(i / L) if path else f0 * (f1 / f0) ** (i / L)
        out.append(b.bandpass(random.random() * 2 - 1, f, q))
    return out


def sweep(dur, f0, f1, shape=2.0):
    out, ph = [], 0.0
    L = n(dur)
    for i in range(L):
        f = f0 * (f1 / f0) ** ((i / L) ** (1 / shape))
        ph += 2 * math.pi * f / SR
        out.append(math.sin(ph) + 0.3 * math.sin(2 * ph))
    return out


def reverb(x, wet=0.3, tail=0.6):
    """Four feedback combs + a lowpass smear; enough air for short UI sounds."""
    delays = [0.0297, 0.0371, 0.0411, 0.0437]
    L = len(x) + n(tail)
    xs = x + [0.0] * n(tail)
    out = [0.0] * L
    for d in delays:
        D = n(d)
        fb = 0.001 ** (d / tail)
        buf = [0.0] * L
        lp = 0.0
        for i in range(L):
            fbv = buf[i - D] if i >= D else 0.0
            lp = 0.6 * lp + 0.4 * fbv
            buf[i] = xs[i] + fb * lp
            out[i] += buf[i] * 0.25
    return [a * (1 - wet) + b * wet for a, b in zip(xs, out)]


def crush(x, bits=6, hold=3):
    q = 2 ** bits
    out, last = [], 0.0
    for i, v in enumerate(x):
        if i % hold == 0:
            last = round(v * q) / q
        out.append(last)
    return out


def rev(x):
    return x[::-1]


def fade(x, a=0.004, r=0.02):
    return apply(x, env_adsr(len(x), a, 0, 1, r))


def write(name, x, peak=0.85):
    if name in KEEP and os.path.exists(os.path.join(OUT, f"{name}.wav")):
        return
    m = max(1e-9, max(abs(v) for v in x))
    x = [v * peak / m for v in x]
    # trim the inaudible end of the reverb tail (below about -50 dB)
    last = max((i for i, v in enumerate(x) if abs(v) > 0.0015), default=len(x) - 1)
    x = x[: min(len(x), last + n(0.02))]
    tail = min(n(0.15), len(x) // 3)  # ease out over the last 150 ms, curved so it never sounds cut
    x = [v * (((len(x) - i) / tail) ** 2 if i > len(x) - tail else 1) for i, v in enumerate(x)]
    x = fade(x, 0.004, 0.001)
    with wave.open(os.path.join(OUT, f"{name}.wav"), "w") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SR)
        w.writeframes(b"".join(struct.pack("<h", int(max(-1, min(1, v)) * 32767)) for v in x))
    print(f"{name}.wav  {len(x) / SR:.2f}s")


# ------------------------------------------------------------------ the pack (v2)
# Feedback on v1: 1 disliked; 2 and 3 "off"; 4 too ting-y; 6 and 9 too much bell; 11 softer;
# 12 weird. Liked: 5, 7, 8, 10, 13. The "off"/"weird" ones shared a ring-mod shimmer
# (inharmonic sidebands) and inharmonic FM bells, so v2 uses neither.

# 1 approval, round 3: the user disliked both voice versions; three non-voice options.
# A: a warm two-note rise (G4 -> C5), same timbre as #4.
seed("approval-a")
a1 = tone(392.0, 0.35, (1, 0.3, 0.08), a=0.01, tau=0.18)
a2 = tone(523.3, 0.6, (1, 0.3, 0.08), a=0.01, tau=0.3)
write("tw-approval-a", reverb(mix(a1, a2, at=[0, 0.13]), wet=0.25, tail=0.4))
# B: a portal opening, #10 (closed) run the other way: soft thump, then a rising sweep.
seed("approval-b")
thump_b = apply([math.sin(2 * math.pi * 80 * i / SR) for i in range(n(0.12))], env_exp(n(0.12), 0.035))
# round 7: peaks early then fades, so the bright end of the sweep stays quiet
# round 9: a longer, exponential fade-out so it trails off instead of stopping
B_DUR = 0.45
fade_b = lambda: [min(1, u / 0.17) * math.exp(-max(0, u - 0.17) * 6) * (1 - u) ** 0.5 for u in (i / n(B_DUR) for i in range(n(B_DUR)))]
open_b = apply(noise_sweep(B_DUR, 180, 1000, q=1.8), fade_b())
hint_b = apply(sweep(B_DUR, 160, 420, shape=1.6), fade_b())
# round 8: the thump was too hard (it started at full level). Rounded: a 25 ms raised-cosine
# fade in, then a slow decay, with the pitch easing down from 85 to 65 Hz.
def round_thump(dur=0.11, attack=0.025, f0=85, f1=65, tau=0.04):
    out, ph = [], 0.0
    for i in range(n(dur)):
        t = i / SR
        ph += 2 * math.pi * (f0 + (f1 - f0) * t / dur) / SR
        rise = 0.5 - 0.5 * math.cos(math.pi * min(1, t / attack))
        out.append(math.sin(ph) * rise * math.exp(-max(0, t - attack) / tau))
    return out
write("tw-approval-b", reverb(mix(gain(round_thump(), 0.06), gain(open_b, 0.55), gain(hint_b, 0.08), at=[0, 0.02, 0.02]), wet=0.3, tail=0.7), peak=0.55)
# C: a soft double knock (wooden: fundamental + a 4th harmonic, very fast decay).
seed("approval-c")
knock = lambda f: tone(f, 0.12, (1, 0, 0, 0.35), a=0.001, tau=0.03)
write("tw-approval-c", reverb(mix(knock(330), knock(392), at=[0, 0.13]), wet=0.2, tail=0.3))

# 2 attention: same two-note call (minor third), now pure in-tune tones; the reversed swell
# is the first note's own reverb run backwards, so it lands exactly on the note.
seed("attention")
note1 = tone(1047, 0.9, (1, 0.35, 0.12), tau=0.14)
note2 = tone(1245, 1.1, (1, 0.35, 0.12), tau=0.17)
swell = rev(reverb(tone(1047, 0.15, (1, 0.3), tau=0.05), wet=0.9, tail=0.15))
write("tw-attention", reverb(mix(gain(swell, 0.45), note1, gain(note2, 0.9), at=[0, len(swell) / SR, len(swell) / SR + 0.14]), wet=0.25, tail=0.5))

# 3 failed: a voiced "uh-oh", stepped down a minor third, a soft low tone under each syllable.
seed("failed")
uhoh = gibber([("a", 1.0, 1.4), ("o", 0.84, 2.2)], base=380, glide=0.0, breath=0.05)
low = mix(tone(190, 0.18, (1, 0.2), a=0.01, tau=0.12), tone(160, 0.7, (1, 0.2), a=0.01, tau=0.2), at=[0, 0.11])
write("tw-failed", reverb(mix(uhoh, gain(low, 0.35)), wet=0.2, tail=0.5))

# 4 turn_done: a warm, low strummed chord (C E G) with a soft breath of air, no high bells.
seed("done")
air = apply(noise_sweep(0.2, 500, 1800, q=1.2), env_adsr(n(0.2), 0.14, 0, 1, 0.06))
chord = mix(*[tone(f, 1.4, (1, 0.25, 0.06), a=0.02, tau=0.22) for f in (261.6, 329.6, 392.0, 523.3)], at=[0, 0.035, 0.07, 0.105])
write("tw-done", reverb(mix(gain(air, 0.12), chord, at=[0, 0.12]), wet=0.25, tail=0.5))

# 6 approved, round 5: the user disliked the voice (yip, mm-hm) and the whoosh-pop. Three
# options built from sounds they liked: C a tiny #4 (two warm notes up), D a soft rising
# bloop, E a mini #1 (the breath, no thump).
seed("approved-c")
write("tw-approved-c", reverb(mix(tone(523.3, 0.5, (1, 0.25, 0.06), a=0.008, tau=0.08), tone(784.0, 0.7, (1, 0.25, 0.06), a=0.008, tau=0.12), at=[0, 0.07]), wet=0.2, tail=0.3), peak=0.6)
seed("approved-d")
bloop = apply(sweep(0.12, 380, 620, shape=0.7), env_adsr(n(0.12), 0.008, 0.04, 0.6, 0.06))
write("tw-approved-d", reverb(bloop, wet=0.18, tail=0.25), peak=0.55)
seed("approved-e")
write("tw-approved-e", reverb(apply(noise_sweep(0.16, 220, 1300, q=1.8), env_adsr(n(0.16), 0.04, 0.03, 0.75, 0.07)), wet=0.22, tail=0.25), peak=0.5)

# 9 image_added, round 6: slow and soft was right, but it shouldn't rise at the end.
# Same "fwip" body three ways: A falls, B rises a little then settles, C stays level.
fw_env = lambda: env_adsr(n(0.28), 0.17, 0, 1, 0.09)
seed("sparkle-a")
write("tw-sparkle-a", reverb(apply(noise_sweep(0.28, 2600, 700, q=2.0), fw_env()), wet=0.2, tail=0.3), peak=0.42)
seed("sparkle-b")
arc = lambda u: 900 * (2.2 ** math.sin(math.pi * u))  # 900 Hz -> ~2 kHz at the middle -> 900 Hz
write("tw-sparkle-b", reverb(apply(noise_sweep(0.28, 0, 0, q=2.0, path=arc), fw_env()), wet=0.2, tail=0.3), peak=0.42)
seed("sparkle-c")
write("tw-sparkle-c", reverb(apply(noise_sweep(0.28, 1400, 1400, q=2.0), fw_env()), wet=0.2, tail=0.3), peak=0.42)

# 11 switched: a soft, rounded tick, lower and quieter.
seed("tick")
write("tw-tick", reverb(tone(1400, 0.06, (1, 0.1), a=0.003, tau=0.012), wet=0.12, tail=0.12), peak=0.4)

# 12 command_bar, round 4: "doesn't feel right". A: a soft tick (the #11/#13 family).
# B: a darker breath, no tone. C: a tiny portal blip, a small cousin of 1B.
seed("portal-a")
write("tw-portal-a", reverb(mix(tone(1100, 0.06, (1, 0.1), a=0.003, tau=0.012), tone(1650, 0.06, (1, 0.1), a=0.003, tau=0.012), at=[0, 0.045]), wet=0.12, tail=0.12), peak=0.4)
seed("portal-b")
write("tw-portal-b", reverb(apply(noise_sweep(0.16, 250, 900, q=2.2), env_adsr(n(0.16), 0.07, 0.03, 0.7, 0.05)), wet=0.2, tail=0.15), peak=0.4)
seed("portal-c")
blip_t = apply([math.sin(2 * math.pi * 90 * i / SR) for i in range(n(0.08))], env_exp(n(0.08), 0.025))
blip_n = apply(noise_sweep(0.14, 250, 1200, q=1.8), env_adsr(n(0.14), 0.04, 0.03, 0.7, 0.06))
write("tw-portal-c", reverb(blip_t, wet=0.2, tail=0.2), peak=0.45)

# ------------------------------------------------------------------ first-round recipes
# Approved in round 1 and never re-rendered. They first came out of one shared random
# sequence, so these blocks make close relatives, not the exact shipped files (README.md).

# agent / from_trigger: a quick "hm!" chirp, friendly.  (ships as Hm)
seed("ping")
write("tw-ping", reverb(gibber([("u", 1.1, 0.9), ("a", 1.35, 1.2)], base=520, glide=0.1), wet=0.25))

# denied: a little downward "nn-nn".  (ships as Nn-nn)
seed("denied")
write("tw-denied", reverb(gibber([("u", 1.0, 0.9), ("o", 0.8, 1.1)], base=430, glide=-0.25, breath=0.08), wet=0.15, tail=0.3))

# queue_sent: airy whoosh into a glassy pop.  (ships as Whoosh)
seed("sent")
whoosh = apply(noise_sweep(0.22, 600, 5000, q=2), env_adsr(n(0.22), 0.15, 0.0, 1, 0.05))
write("tw-sent", reverb(mix(gain(whoosh, 0.35), bell(1760, 0.3, tau=0.09, ratio=2.0), at=[0, 0.19]), wet=0.2, tail=0.35))

# closed: a portal shutting, a downward sweep collapsing into a soft thump.  (ships as Close)
seed("closed")
close = apply(sweep(0.25, 900, 120, shape=0.6), env_exp(n(0.25), 0.1))
thump = apply([math.sin(2 * math.pi * 70 * i / SR) for i in range(n(0.15))], env_exp(n(0.15), 0.04))
write("tw-closed", reverb(mix(gain(close, 0.5), thump, at=[0, 0.12]), wet=0.25, tail=0.4))

# copied: two soft ticks.  (ships as Tick-tick)
seed("copy")
write("tw-copy", reverb(mix(bell(2637, 0.07, tau=0.015, index=1.0), bell(3520, 0.07, tau=0.015, index=1.0), at=[0, 0.06]), wet=0.15, tail=0.15))
