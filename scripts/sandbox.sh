#!/usr/bin/env bash
# Builds the scratch library `just sandbox` makes: a downloads folder and a
# media folder of sparse files, so a 40 GiB film takes a few MiB of disk.
#
#   scripts/sandbox.sh <sandbox dir> <second-disk dir or "">
#
# The second-disk dir (on /dev/shm, a different filesystem) holds part of the
# library. `just try` bind-mounts it into the media folder inside a private
# mount namespace, so those library files sit on another filesystem than
# their downloads and the link falls back to a symlink, as on a mergerfs
# pool. Its mounts are listed in <sandbox>/.second-disk for `just try`.
#
# Every duplicate pair is read whole by the scan, at roughly 0.5 GB/s in a
# debug build, so the duplicates are kept to about 55 GB in total. The very
# large sizes go on files a scan never reads whole: library-only files,
# same-size files whose first MiB differs, and files it cannot open.
set -euo pipefail

sandbox="$1"
shm="$2"
d="$sandbox/downloads"
m="$sandbox/media"

# Start over. The read-only folder and unreadable file need their permissions
# back before they can be removed.
for dir in "$sandbox" "$shm"; do
    if [ -n "$dir" ] && [ -e "$dir" ]; then
        chmod -R u+rwX "$dir"
        rm -rf "$dir"
    fi
done
mkdir -p "$d" "$m"

mib=$((1024 * 1024))

# A sparse file of $2 MiB with random first and last MiB, so no two files
# made here match unless one is copied from the other.
fake() {
    local path="$1" size="$2"
    mkdir -p "$(dirname "$path")"
    truncate -s "${size}M" "$path"
    head -c "$mib" /dev/urandom | dd of="$path" conv=notrunc status=none
    if [ "$size" -gt 1 ]; then
        head -c "$mib" /dev/urandom | dd of="$path" bs="$mib" seek=$((size - 1)) conv=notrunc status=none
    fi
}

# A copy that stays sparse.
copy() {
    mkdir -p "$(dirname "$2")"
    cp --sparse=always "$1" "$2"
}

# A copy whose first MiB differs: the quick hash already tells them apart.
differs_at_start() {
    copy "$1" "$2"
    head -c "$mib" /dev/urandom | dd of="$2" conv=notrunc status=none
}

# A copy that differs only in its middle: the quick hash (first and last MiB)
# matches, and only the full hash tells them apart.
differs_in_middle() {
    copy "$1" "$2"
    local size
    size=$(stat -c %s "$1")
    head -c 65536 /dev/urandom | dd of="$2" bs=65536 seek=$((size / 2 / 65536)) conv=notrunc status=none
}

# A small real file, under the 1 MiB floor: .nfo, subtitles, artwork.
small() {
    mkdir -p "$(dirname "$1")"
    head -c "$2" /dev/urandom > "$1"
}

episode() { printf 'S%02dE%02d' "$1" "$2"; }

# ── Films on the main disk ────────────────────────────────────────────────

# Hardlinked: one download per film.
lib="$m/Movies/The Long Orbit (2019)/The Long Orbit (2019).mkv"
fake "$lib" 4096
rel="$d/The.Long.Orbit.2019.1080p.BluRay.x264"
copy "$lib" "$rel/The.Long.Orbit.2019.1080p.BluRay.x264.mkv"
small "$rel/The.Long.Orbit.2019.1080p.BluRay.x264.nfo" 4000
fake "$rel/Sample/the.long.orbit.sample.mkv" 60

lib="$m/Movies/Harbour Lights (2021)/Harbour Lights (2021).mkv"
fake "$lib" 2048
copy "$lib" "$d/Harbour.Lights.2021.1080p.WEB-DL/Harbour.Lights.2021.1080p.WEB-DL.mkv"
small "$m/Movies/Harbour Lights (2021)/poster.jpg" 300000

# Hardlinked twice: the same release in the downloads folder and in a
# cross-seed folder.
lib="$m/Movies/Glass Meridian (2017)/Glass Meridian (2017).mkv"
fake "$lib" 3072
copy "$lib" "$d/Glass.Meridian.2017.2160p.Remux/Glass.Meridian.2017.2160p.Remux.mkv"
copy "$lib" "$d/cross-seed/Glass.Meridian.2017.2160p.Remux.mkv"

# Already one file: the download is a hardlink of the library file.
lib="$m/Movies/Quiet Valley (2015)/Quiet Valley (2015).mkv"
fake "$lib" 40960
mkdir -p "$d/Quiet.Valley.2015.2160p.UHD"
ln "$lib" "$d/Quiet.Valley.2015.2160p.UHD/Quiet.Valley.2015.2160p.UHD.mkv"

# Same size, different content: never linked.
lib="$m/Movies/Northbound (2020)/Northbound (2020).mkv"
fake "$lib" 47104
differs_at_start "$lib" "$d/Northbound.2020.1080p.BluRay/Northbound.2020.1080p.BluRay.mkv"
lib="$m/Movies/Paper Kingdoms (2022)/Paper Kingdoms (2022).mkv"
fake "$lib" 2048
differs_in_middle "$lib" "$d/Paper.Kingdoms.2022.1080p.WEB/Paper.Kingdoms.2022.1080p.WEB.mkv"

# In the library only.
fake "$m/Movies/Saltwater (2018)/Saltwater (2018).mkv" 61440

# Fails to link: its download folder is read-only, so no link can be made
# beside the download.
lib="$m/Movies/Ember Road (2023)/Ember Road (2023).mkv"
fake "$lib" 1024
locked="$d/Ember.Road.2023.1080p.AMZN.WEB-DL"
copy "$lib" "$locked/Ember.Road.2023.1080p.AMZN.WEB-DL.mkv"
chmod 555 "$locked"

# Cannot be read: the scan reports it and moves on.
lib="$m/Movies/Night Ferry (2016)/Night Ferry (2016).mkv"
fake "$lib" 15360
copy "$lib" "$d/Night.Ferry.2016.720p.HDTV/Night.Ferry.2016.720p.HDTV.mkv"
chmod 000 "$d/Night.Ferry.2016.720p.HDTV/Night.Ferry.2016.720p.HDTV.mkv"

# A symlink left from an earlier run whose library file has since gone.
mkdir -p "$d/Old.Signal.2012.1080p"
ln -s "../../media/Movies/Old Signal (2012)/Old Signal (2012).mkv" \
    "$d/Old.Signal.2012.1080p/Old.Signal.2012.1080p.mkv"

# ── Series on the main disk ───────────────────────────────────────────────

# A season pack, every episode imported: eight hardlinks.
pack="$d/Lantern.Street.S01.1080p.WEB-DL"
for e in 1 2 3 4 5 6 7 8; do
    ep=$(episode 1 "$e")
    lib="$m/TV/Lantern Street/Season 1/Lantern Street - $ep.mkv"
    fake "$lib" 512
    copy "$lib" "$pack/Lantern.Street.$ep.1080p.WEB-DL.mkv"
    small "$pack/Subs/Lantern.Street.$ep.en.srt" 45000
done

# Single-episode releases. E03 is imported but its download was removed; E04
# is downloaded but not imported yet.
for e in 1 2 3; do
    fake "$m/TV/Lantern Street/Season 2/Lantern Street - $(episode 2 "$e").mkv" 512
done
for e in 1 2; do
    ep=$(episode 2 "$e")
    copy "$m/TV/Lantern Street/Season 2/Lantern Street - $ep.mkv" \
        "$d/Lantern.Street.$ep.1080p.WEB-DL/Lantern.Street.$ep.1080p.WEB-DL.mkv"
done
fake "$d/Lantern.Street.S02E04.1080p.WEB-DL/Lantern.Street.S02E04.1080p.WEB-DL.mkv" 512

# A pack where the library holds a different encode of one episode: five
# hardlinks, and E04 stays.
pack="$d/The.Cartographers.S01.2160p.WEB-DL"
for e in 1 2 3 4 5 6; do
    ep=$(episode 1 "$e")
    lib="$m/TV/The Cartographers/Season 1/The Cartographers - $ep.mkv"
    fake "$lib" 768
    if [ "$e" = 4 ]; then
        differs_at_start "$lib" "$pack/The.Cartographers.$ep.2160p.WEB-DL.mkv"
    else
        copy "$lib" "$pack/The.Cartographers.$ep.2160p.WEB-DL.mkv"
    fi
done

# Already linked by an earlier run: six hardlinks, nothing to do.
pack="$d/Small.Hours.S01.720p.WEB"
mkdir -p "$pack"
for e in 1 2 3 4 5 6; do
    ep=$(episode 1 "$e")
    lib="$m/TV/Small Hours/Season 1/Small Hours - $ep.mkv"
    fake "$lib" 350
    ln "$lib" "$pack/Small.Hours.$ep.720p.WEB.mkv"
done

# ── Downloads with no library copy ────────────────────────────────────────

for n in 1 2 3; do
    fake "$d/Deep.Water.Documentary.Collection/Part.$n.1080p.mkv" 2048
done
fake "$d/incomplete/Upcoming.Film.2024.2160p.mkv.part" 20480
mkdir -p "$d/incomplete"
: > "$d/incomplete/Upcoming.Film.2024.2160p.nfo.part"
: > "$d/incomplete/empty-placeholder.mkv"

# ── Library on the second disk ────────────────────────────────────────────

if [ -z "$shm" ]; then
    echo "No second disk: /dev/shm, setpriv or a private mount namespace is missing, so no row will become a symlink." >&2
    exit 0
fi

# Each folder is made on /dev/shm and mounted over an empty folder of the
# same name in the library. Its downloads stay on the main disk.
mounts="$sandbox/.second-disk"
: > "$mounts"
second_disk() {
    mkdir -p "$shm/$1" "$m/$1"
    printf '%s\t%s\n' "$shm/$1" "$m/$1" >> "$mounts"
}

second_disk "Movies/Far Shore (2014)"
lib="Movies/Far Shore (2014)/Far Shore (2014).mkv"
fake "$shm/$lib" 3072
copy "$shm/$lib" "$d/Far.Shore.2014.1080p.BluRay/Far.Shore.2014.1080p.BluRay.mkv"

second_disk "TV/Moonlit Archive"
pack="$d/Moonlit.Archive.S01.1080p.WEB-DL"
for e in 1 2 3 4 5 6; do
    ep=$(episode 1 "$e")
    lib="TV/Moonlit Archive/Season 1/Moonlit Archive - $ep.mkv"
    fake "$shm/$lib" 384
    copy "$shm/$lib" "$pack/Moonlit.Archive.$ep.1080p.WEB-DL.mkv"
done
