#!/bin/bash
# Fictional music for the website screenshots: synthesized (kick, hats, snare, bass, pad), tagged, with generated covers.
# usage: make-showcase.sh OUTDIR
set -euo pipefail
OUT=${1:?outdir}
W=$OUT/.work
mkdir -p "$OUT" "$W"
ff() { ffmpeg -y -v error "$@"; }

# synth BPM F0 SECONDS OUT.wav SAMPLERATE
synth() {
  local bpm=$1 f0=$2 T=$3 out=$4 sr=$5
  local P; P=$(awk "BEGIN{print 60/$bpm}")
  local expr="clip( \
    (sin(2*PI*(48+110*exp(-mod(t,$P)*28))*mod(t,$P))*exp(-mod(t,$P)*7)*0.9 \
     + sin(2*PI*$f0*0.5*t)*exp(-mod(t+$P/2,$P)*8)*0.55 ) * if(between(t,0.44*$T,0.58*$T),0,1) * min(1,t/(0.08*$T)) \
    + (random(0)*2-1)*exp(-mod(t+$P/2,$P)*45)*0.2 * if(between(t,0.44*$T,0.50*$T),0.3,1) \
    + (random(1)*2-1)*exp(-if(gte(mod(t,2*$P)-$P,0),mod(t,2*$P)-$P,9)*20)*0.32 * if(between(t,0.44*$T,0.58*$T),0,1) \
    + (random(1)*2-1)*0.06*pow(max(0,(t-0.5*$T)/(0.08*$T)),2)*between(t,0.5*$T,0.58*$T) \
    + 0.07*(sin(2*PI*$f0*2*t)+sin(2*PI*$f0*2*1.26*t)+sin(2*PI*$f0*2*1.5*t))*(0.7+0.5*between(t,0.44*$T,0.58*$T)), -0.98, 0.98)"
  expr=$(echo "$expr" | tr -d '\n' | tr -s ' ')
  ff -f lavfi -i "aevalsrc='$expr':s=$sr:d=$T" "$out"
}

# Plasma covers (sums of sines, one phase per colour channel); they have structure, so they still read once the deck dithers them.
cover() { # NAME SCALE P1 P2 P3 [rings]
  local n=$1 k=$2 p1=$3 p2=$4 p3=$5
  local r="128+127*sin(X/(75*$k)+$p1+sin(Y/(190*$k))*2.2)"
  local g="128+127*sin(Y/(62*$k)+$p2+sin(X/(170*$k))*2.2)"
  local b="128+127*sin((X+Y)/(130*$k)+$p3+sin((X-Y)/(220*$k))*1.8)"
  if [ "${6:-}" = rings ]; then
    r="128+127*sin(hypot(X-400,Y-400)/(14*$k)+$p1)"; g="128+127*sin(hypot(X-400,Y-400)/(14*$k)+$p2+X/300)"; b="128+127*sin(hypot(X-400,Y-400)/(14*$k)+$p3+Y/300)"
  fi
  ff -f lavfi -i "color=c=black:s=800x800:r=1:d=1,format=gbrp,geq=r='$r':g='$g':b='$b',hue=s=0.55" -frames:v 1 -q:v 2 "$W/$n.jpg"
}

# ---- covers
cover mix 1.0 0.0 2.1 4.2 rings
cover c1 1.0 0.5 1.7 3.1
cover c2 1.3 2.0 0.4 5.0
cover c3 0.8 4.0 3.3 0.9
cover c4 1.1 1.0 5.5 2.2
cover c5 0.9 3.5 0.2 1.3
cover c6 1.4 5.1 2.6 4.4
cover c7 0.7 0.8 4.4 2.9
cover c8 1.2 2.7 1.1 0.3
cover c9 1.0 4.6 5.2 3.8

# ---- the mix: six tracks, 10 s blends
names=("Glass Harbour" "Night Shift (Extended Mix)" "Low Tide (Dub Mix)" "Parallel Lines (Kestrel Remix)" "Sunday Static" "Closing Hours")
artists=("Halden Rue" "Tessellate" "Orla Finch & Nova Kite" "Static Orchard" "Pale Harbour" "Marlow Vane")
bpms=(122 124 124 126 126 124)
f0s=(110 123.47 130.81 146.83 164.81 110)
lens=(270 300 330 280 300 260)
OV=10
inputs=(); for i in 0 1 2 3 4 5; do
  synth "${bpms[$i]}" "${f0s[$i]}" "${lens[$i]}" "$W/sec$i.wav" 44100
  inputs+=(-i "$W/sec$i.wav")
done
fc="[0][1]acrossfade=d=$OV:c1=tri:c2=tri[a1]"
for i in 2 3 4 5; do fc+=";[a$((i-1))][$i]acrossfade=d=$OV:c1=tri:c2=tri[a$i]"; done
fc+=";[a5]pan=stereo|c0=c0|c1=0.97*c0,volume=1.1[out]"
ff "${inputs[@]}" -i "$W/mix.jpg" -filter_complex "$fc" -map "[out]" -map 6:v -c:v copy -c:a libmp3lame -b:a 256k -ar 44100 \
  -id3v2_version 3 -metadata title="Sunday Session" -metadata artist="Mara Voss" -metadata date="2025" -metadata genre="House" \
  -metadata:s:v title="Cover" -metadata:s:v comment="Cover (front)" "$OUT/00 Mara Voss - Sunday Session.mp3"

# cue sheet next to the file: a track takes over in the middle of each blend
cue="$OUT/00 Mara Voss - Sunday Session.cue"
{
  echo 'PERFORMER "Mara Voss"'; echo 'TITLE "Sunday Session"'; echo 'FILE "00 Mara Voss - Sunday Session.mp3" MP3'
  start=0
  for i in 0 1 2 3 4 5; do
    at=$start; if [ "$i" -gt 0 ]; then at=$((start + OV / 2)); fi
    printf '  TRACK %02d AUDIO\n    TITLE "%s"\n    PERFORMER "%s"\n    INDEX 01 %02d:%02d:00\n' $((i+1)) "${names[$i]}" "${artists[$i]}" $((at/60)) $((at%60))
    start=$((start + ${lens[$i]} - OV))
  done
} > "$cue"

# ---- single tracks: 24 s of material looped to a real length, tagged
single() { # N ARTIST TITLE ALBUM YEAR BPM KEY MINUTES SECONDS COVER
  local n=$1 artist=$2 title=$3 album=$4 year=$5 bpm=$6 key=$7 dur=$(( $8 * 60 + $9 )) cov=${10}
  local f0; f0=$(awk "BEGIN{print 55*2^(($RANDOM%12)/12)*2}")
  synth "$bpm" "$f0" 24 "$W/s$n.wav" 22050
  ff -stream_loop -1 -i "$W/s$n.wav" -i "$W/$cov.jpg" -t "$dur" -map 0:a -map 1:v -c:v copy -c:a libmp3lame -b:a 64k -ac 1 \
    -id3v2_version 3 -metadata title="$title" -metadata artist="$artist" -metadata album="$album" -metadata date="$year" \
    -metadata TBPM="$bpm" -metadata TKEY="$key" -metadata genre="House" \
    -metadata:s:v title="Cover" -metadata:s:v comment="Cover (front)" "$OUT/$(printf '%02d' "$n") $artist - $title.mp3"
}
single 1 "Marlow Vane"     "Closing Hours (Original Mix)"  "Last Light EP"  2025 124 8A  6 48 c1
single 2 "Tessellate"      "Night Shift (Extended Mix)"    "Night Shift"    2024 124 9A  7 22 c2
single 3 "Halden Rue"      "Glass Harbour"                 "Tidal"          2025 122 5A  6 15 c3
single 4 "Orla Finch"      "Paper Moons"                   "Paper Moons"    2023 128 11B 5 40 c4
single 5 "Nova Kite"       "Low Tide"                      "Low Tide"       2024 118 4A  6 58 c5
single 6 "Static Orchard"  "Parallel Lines"                "Signal Drift"   2025 126 8B  7 5  c6
single 7 "Pale Harbour"    "Sunday Static"                 "Slow Weather"   2024 120 10A 6 32 c7
single 8 "Kestrel"         "Halcyon"                       "Halcyon"        2023 125 7A  7 41 c8
single 9 "Juno Marsh"      "Afterglow (Dub)"               "Afterglow"      2025 123 12B 6 4  c9

rm -rf "$W"
ls -la "$OUT"
