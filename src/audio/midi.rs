//! Standard MIDI File (`.mid`) playback on reserved libSiON tracks.
//!
//! libSiON has no MIDI sequencer and its MML text cannot express mid-note
//! bends, so the work is split in two: a self-contained SMF parser turns
//! the file into per-channel note + control streams ([`MidiSong::parse`]),
//! and [`MidiPlayer`] drives reserved `USER_CONTROLLED` tracks in realtime
//! from the app's audio pump — notes sample-accurate via
//! [`SiMMLTrack::key_on`], controls (pitch bend, expression, volume) at
//! block granularity via the track setters.
//!
//! Mechanics worth remembering:
//! - Every rendered driver block is preceded by one engine pump, so a
//!   one-block lookahead is enough for sample-accurate note fires: the
//!   `key_on` delay is consumed by the very next block.
//! - While a `key_on` delay is pending the track channel is frozen
//!   (silence), so notes are armed at the *last pump* before their start —
//!   a still-ringing previous note is cut at most one block early, and
//!   gap-following notes wait for the previous note-off so its release
//!   tail rings naturally.
//! - The executor reads pitch bend / expression / velocity at fire time,
//!   so realtime controls applied between arming and firing still land.
//!
//! Supported: formats 0/1, PPQ tick division (SMPTE is rejected), running
//! status (preserved across meta events, cleared only by sysex — real-world
//! writers rely on both), first-tempo-wins, note on/off pairing (FIFO per
//! channel+key, unclosed notes clipped at the end), GM program per channel,
//! CC7 volume, CC11 expression, pitch bend with the RPN 0,0 bend-range
//! negotiation (`CC101=0, CC100=0, CC6=n`, default ±2 semitones). Data
//! entry after an NRPN selection (CC99/CC98) is ignored. Channel 10 (index
//! 9, GM drums) is parsed but skipped at playback — drum kits are out of
//! scope, as are tempo changes after the first, CC64+ controllers and
//! >1-note polyphony per channel.
//!
//! Reserved tracks survive screen switches: the engine only drops the
//! player when a different track starts, and reservations happen after
//! `driver.stream(true)` because `prepare_process` frees every track.

use std::cell::RefCell;
use std::rc::Rc;

use libsion::utils::voice_preset_util::{SiONVoicePresetUtil, INCLUDE_MIDI};
use libsion::{SiONDriver, SiONVoice, TrackRc};

use super::SAMPLE_RATE;

/// libSiON parser resolution: 1920 ticks per whole note (480 per quarter).
const RESOLUTION: f64 = 1920.0;
/// GM default pitch-bend range (±2 semitones) until RPN 0,0 says otherwise.
const DEFAULT_BEND_RANGE: f32 = 2.0;
/// GM drum channel index: parsed for completeness, skipped at playback.
const DRUM_CHANNEL: u8 = 9;
/// Velocity→track-velocity scale (`set_velocity` spans 0..512, MIDI 1..127).
const VELOCITY_SCALE: f32 = 4.0;
/// CC7→master-volume span (`set_master_volume` spans 0..128, MIDI 0..127).
const VOLUME_SPAN: f32 = 128.0;
/// GM CC7 fallback volume when a channel never sends one.
const DEFAULT_VOLUME: u8 = 100;

/// True for extensions this bus owns (`assets/**.mid/.midi`).
pub fn is_midi_path(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("mid") || ext.eq_ignore_ascii_case("midi"))
}

// ---------------------------------------------------------------------------
// Song model (parse output)
// ---------------------------------------------------------------------------

/// One parsed note-on→note-off pair.
#[derive(Debug, Clone)]
pub struct MidiNote {
    pub note: i32,
    pub velocity: u8,
    /// Absolute tick of the note-on.
    pub start_tick: u32,
    /// Absolute tick of the note-off (or end-clip); a following note whose
    /// `start_tick <= end_tick` joins legato.
    pub end_tick: u32,
}

/// A realtime control change; applied at block granularity during playback.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MidiControl {
    /// 14-bit pitch bend (0..16383, 8192 = center).
    Bend(i32),
    /// CC11 channel expression (0..127).
    Expression(u8),
    /// CC7 channel volume (0..127).
    Volume(u8),
    /// RPN 0,0 data entry: pitch-bend range in semitones.
    BendRange(f32),
}

/// Absolute-tick control event.
#[derive(Debug, Clone, Copy)]
pub struct MidiControlEvent {
    pub tick: u32,
    pub control: MidiControl,
}

/// Everything one MIDI channel needs at playback: its GM program and the
/// two playback cursors' source streams (notes sorted by start, controls
/// sorted by tick).
#[derive(Debug, Clone, Default)]
pub struct MidiChannel {
    /// MIDI channel index 0..16 (10 = drums, unused at playback).
    pub channel: u8,
    /// Last program change value (GM instrument 0..127).
    pub program: u8,
    pub notes: Vec<MidiNote>,
    pub controls: Vec<MidiControlEvent>,
}

/// A parsed `.mid` file: tempo, resolution and the melodic channels.
#[derive(Debug)]
pub struct MidiSong {
    /// First tempo meta event, in beats per minute (120 when absent).
    pub bpm: f64,
    /// Ticks per quarter note from the MThd header.
    pub ppq: u16,
    /// Last used tick (note-offs included) — the loop length.
    pub length_ticks: u32,
    /// Melodic channels in ascending channel order; note-less ones dropped.
    pub channels: Vec<MidiChannel>,
}

/// RPN/NRPN selector state of one channel parse: data entry (CC6) applies
/// to whichever controller was selected last; only RPN 0,0 belongs here.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
enum RpnSelection {
    /// RPN 0,0 selected: next CC6 is a bend-range change.
    #[default]
    NotOurs,
    BendRange,
}

/// Mutable parse workspace for one MIDI channel.
#[derive(Default)]
struct ChannelBuilder {
    program: u8,
    notes: Vec<MidiNote>,
    controls: Vec<MidiControlEvent>,
    /// Pairing queues: note-on events waiting for their matching off, FIFO
    /// per key so repeated same-key ons pair in trigger order.
    open: Vec<Vec<(u32, u8)>>,
    rpn: RpnSelection,
}

impl ChannelBuilder {
    fn new() -> Self {
        ChannelBuilder { open: vec![Vec::new(); 128], ..Default::default() }
    }

    fn note_on(&mut self, note: u8, velocity: u8, tick: u32) {
        self.open[note as usize].push((tick, velocity));
    }

    /// Pair the earliest open note-on for `note` with this off.
    fn note_off(&mut self, note: u8, tick: u32) {
        if !self.open[note as usize].is_empty() {
            let (start, velocity) = self.open[note as usize].remove(0);
            self.notes.push(MidiNote {
                note: note as i32,
                velocity,
                start_tick: start,
                end_tick: tick.max(start),
            });
        }
    }

    /// Close any still-open note-ons at the track end (malformed files).
    fn clip_open(&mut self, tick: u32) {
        for (key, queue) in self.open.iter_mut().enumerate() {
            while let Some((start, velocity)) = queue.pop() {
                self.notes.push(MidiNote {
                    note: key as i32,
                    velocity,
                    start_tick: start,
                    end_tick: tick.max(start),
                });
            }
        }
    }

    fn control(&mut self, cc: u8, value: u8, tick: u32) {
        match cc {
            0x06 => {
                // Data entry MSB: bend range iff RPN (0,0) is selected.
                if self.rpn == RpnSelection::BendRange {
                    self.controls.push(MidiControlEvent {
                        tick,
                        control: MidiControl::BendRange(value as f32),
                    });
                }
            }
            0x07 => self
                .controls
                .push(MidiControlEvent { tick, control: MidiControl::Volume(value) }),
            0x0B => self.controls.push(MidiControlEvent {
                tick,
                control: MidiControl::Expression(value),
            }),
            0x64 => {
                // RPN LSB: (0,·) = pitch-bend range selection here; any
                // other parameter number is not ours (its MSB via CC65
                // keeps/forces NotOurs below).
                self.rpn = if value == 0 { RpnSelection::BendRange } else { RpnSelection::NotOurs };
            }
            0x65 => {
                // RPN MSB: only the bend-range RPN has MSB 0; every other
                // MSB targets a parameter we ignore.
                if value != 0 {
                    self.rpn = RpnSelection::NotOurs;
                }
            }
            // NRPN selection: data entry from here on is not ours.
            0x62 | 0x63 => self.rpn = RpnSelection::NotOurs,
            _ => {}
        }
    }
}

/// Same-tick control ordering: range changes before bends so a range
/// update never eats a bend scheduled at the same tick.
fn control_rank(c: MidiControl) -> u8 {
    match c {
        MidiControl::BendRange(_) => 0,
        MidiControl::Bend(_) => 1,
        MidiControl::Volume(_) => 2,
        MidiControl::Expression(_) => 3,
    }
}

impl MidiSong {
    /// Parse Standard MIDI File bytes (formats 0 and 1). Errors are
    /// descriptive strings; a malformed container never panics.
    ///
    /// Chunk robustness: the spec says MTrk bodies end with the `FF 2F`
    /// end-of-track meta, and writers routinely get the declared body
    /// length wrong (our demo asset understates every length, its real
    /// bodies running up to the next `MTrk` header). Tracks are therefore
    /// parsed event-wise with `FF 2F` as the terminator, with the declared
    /// length only bounding where the *next* chunk starts for files that
    /// honor it.
    pub fn parse(bytes: &[u8]) -> Result<MidiSong, String> {
        let mut cur = Cursor::new(bytes);

        let mut header = cur.expect_chunk(b"MThd")?;
        let format = header.u16()?;
        let _ntrks = header.u16()?;
        let division = header.u16()?;
        if division & 0x8000 != 0 {
            return Err("SMPTE time division is not supported".to_string());
        }
        if division == 0 {
            return Err("zero PPQ in MThd".to_string());
        }
        if format > 1 {
            return Err(format!("SMF format {format} is not supported (want 0 or 1)"));
        }

        let mut song = MidiSong { bpm: 120.0, ppq: division, length_ticks: 0, channels: Vec::new() };
        let mut builders: Vec<Option<ChannelBuilder>> = (0..16).map(|_| None).collect();
        let mut tempo_seen = false;

        // Walk every remaining chunk; only MTrk feeds the parser, foreign
        // chunk types are skipped by length (per spec).
        while !cur.at_end() {
            let id = cur.chunk_id()?;
            let declared_len = cur.u32()?;
            let body_start = cur.pos;
            let declared_end = body_start + declared_len as usize;
            if declared_end > bytes.len() {
                return Err("chunk extends past end of file".to_string());
            }

            if id != *b"MTrk" {
                cur.pos = declared_end;
                continue;
            }

            // Event stream window: honor the declared length, but a track
            // whose events run past it (broken writer) extends up to the
            // next chunk header. `FF 2F` ends the parse in both cases; the
            // window end only bounds a stream that never terminates.
            let scanned = bytes[declared_end..]
                .windows(4)
                .position(|w| w == *b"MTrk" || w == *b"MThd")
                .map_or(bytes.len(), |at| declared_end + at);
            let window_end = scanned.max(declared_end);
            let body = &bytes[body_start..window_end];
            let (end_tick, consumed) = parse_track(body, &mut builders, &mut song, &mut tempo_seen)?;
            // Healthy files: the FF 2F sat inside the declared body — jump
            // to the next declared chunk. Broken files: the parse consumed
            // up to the next header, which is where the next chunk begins.
            cur.pos = (body_start + consumed).max(declared_end);
            song.length_ticks = song.length_ticks.max(end_tick);
        }

        // Compact builders into playback channels: drums skipped, note-less
        // channels dropped, streams sorted into play order.
        for channel in 0..16u8 {
            let Some(builder) = builders[channel as usize].take() else { continue };
            if channel == DRUM_CHANNEL || builder.notes.is_empty() {
                continue;
            }
            let mut midi_channel = MidiChannel {
                channel,
                program: builder.program,
                notes: builder.notes,
                controls: builder.controls,
            };
            midi_channel.notes.sort_by_key(|n| n.start_tick);
            midi_channel
                .controls
                .sort_by_key(|e| (e.tick, control_rank(e.control)));
            song.channels.push(midi_channel);
        }
        if song.channels.is_empty() {
            return Err("no melodic notes in any channel".to_string());
        }
        Ok(song)
    }
}

/// Parse one MTrk event stream into the channel builders; returns its end
/// tick plus how many bytes the event walk consumed (the caller resumes
/// chunk reading after that).
fn parse_track(
    body: &[u8],
    builders: &mut Vec<Option<ChannelBuilder>>,
    song: &mut MidiSong,
    tempo_seen: &mut bool,
) -> Result<(u32, usize), String> {
    let mut track = Cursor::new(body);
    let mut tick = 0u32;
    let mut running = 0u8;

    while !track.at_end() {
        tick += track.varlen()?;
        match track.event_status(&mut running, song, tempo_seen)? {
            EventHead::Channel(status) => {
                match status & 0xF0 {
                    0x80 | 0x90 => {
                        let (note, velocity) = (track.u8()?, track.u8()?);
                        let builder = builder(builders, status & 0x0F);
                        if status & 0xF0 == 0x90 && velocity > 0 {
                            builder.note_on(note, velocity, tick);
                        } else {
                            builder.note_off(note, tick);
                        }
                    }
                    0xA0 | 0xB0 => {
                        let (hi, lo) = (track.u8()?, track.u8()?);
                        if status & 0xF0 == 0xB0 {
                            builder(builders, status & 0x0F).control(hi, lo, tick);
                        }
                    }
                    0xE0 => {
                        // 14-bit pitch bend: LSB first. 8192 is center.
                        let (lsb, msb) = (track.u8()?, track.u8()?);
                        let value = ((msb as i32) << 7) | lsb as i32;
                        builder(builders, status & 0x0F)
                            .controls
                            .push(MidiControlEvent { tick, control: MidiControl::Bend(value) });
                    }
                    0xC0 => {
                        let program = track.u8()?;
                        builder(builders, status & 0x0F).program = program;
                    }
                    _ => {
                        track.u8()?; // channel pressure: unused 1-byte data
                    }
                }
            }
            EventHead::End => break, // 2F end-of-track meta
            EventHead::System => continue, // meta/sysex consumed; next delta at loop top
        }
    }

    // Unclosed note-ons clip at the track end.
    for slot in builders.iter_mut().flatten() {
        slot.clip_open(tick);
    }
    Ok((tick, track.pos))
}

fn builder<'a>(
    builders: &'a mut Vec<Option<ChannelBuilder>>,
    channel: u8,
) -> &'a mut ChannelBuilder {
    builders[channel as usize].get_or_insert_with(ChannelBuilder::new)
}

/// What [`Cursor::event_status`] resolved for one event.
enum EventHead {
    /// A channel voice message with this status; data bytes follow.
    Channel(u8),
    /// `2F` end-of-track meta: stop reading this track.
    End,
    /// Meta (other than `2F`) or sysex, already consumed: read the next
    /// delta and continue.
    System,
}

// --- byte cursor -------------------------------------------------------------

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, pos: 0 }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn u8(&mut self) -> Result<u8, String> {
        let b = self
            .bytes
            .get(self.pos)
            .copied()
            .ok_or_else(|| "unexpected end of chunk".to_string())?;
        self.pos += 1;
        Ok(b)
    }

    fn u16(&mut self) -> Result<u16, String> {
        Ok(((self.u8()? as u16) << 8) | self.u8()? as u16)
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(((self.u16()? as u32) << 16) | self.u16()? as u32)
    }

    /// VLQ (variable-length quantity) as used for delta times and lengths.
    fn varlen(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for _ in 0..4 {
            let b = self.u8()?;
            value = (value << 7) | (b & 0x7F) as u32;
            if b & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("malformed variable-length quantity".to_string())
    }

    fn take(&mut self, len: u32) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(len as usize)
            .ok_or_else(|| "chunk length overflow".to_string())?;
        if end > self.bytes.len() {
            return Err("chunk extends past end of chunk".to_string());
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    /// Resolve exactly one event: explicit status (high bit set) or running
    /// status (omitted). Channel voice messages report their status with
    /// the data bytes left for the caller; meta and sysex payloads are
    /// consumed here — the `FF 51` tempo payload feeds `song` — and report
    /// [`EventHead::System`] so the caller reads the next delta. Running
    /// status survives meta events (real-world writers rely on it) and is
    /// cleared only by sysex.
    fn event_status(
        &mut self,
        running: &mut u8,
        song: &mut MidiSong,
        tempo_seen: &mut bool,
    ) -> Result<EventHead, String> {
        if self.at_end() {
            return Ok(EventHead::End);
        }
        let byte = self.peek().expect("checked above");
        if byte & 0x80 == 0 {
            // Running status: the status byte is omitted entirely.
            if *running == 0 {
                return Err("data byte without status".to_string());
            }
            return Ok(EventHead::Channel(*running));
        }
        let status = self.u8()?;
        if status & 0xF0 != 0xF0 {
            *running = status;
            return Ok(EventHead::Channel(status));
        }
        match status {
            0xFF => {
                let meta = self.u8()?;
                let len = self.varlen()?;
                let data = self.take(len)?;
                if meta == 0x51 && data.len() == 3 && !*tempo_seen {
                    // Set-tempo meta: first one wins.
                    *tempo_seen = true;
                    let us = ((data[0] as u32) << 16) | ((data[1] as u32) << 8) | data[2] as u32;
                    if us > 0 {
                        song.bpm = 60_000_000.0 / us as f64;
                    }
                }
                if meta == 0x2F {
                    Ok(EventHead::End)
                } else {
                    Ok(EventHead::System)
                }
            }
            0xF0 | 0xF7 => {
                // Sysex: skip the varlen payload; only sysex clears
                // running status.
                let len = self.varlen()?;
                self.take(len)?;
                *running = 0;
                Ok(EventHead::System)
            }
            _ => Err(format!("unexpected system status {status:#04x}")),
        }
    }

    /// Read a `len`-byte chunk body following the 4-byte id at `pos`.
    fn expect_chunk(&mut self, id: &[u8; 4]) -> Result<Cursor<'a>, String> {
        let (chunk_id, body) = (self.chunk_id()?, {
            let len = self.u32()?;
            self.take(len)?
        });
        if &chunk_id != id {
            return Err(format!(
                "expected chunk {:?}, found {:?}",
                String::from_utf8_lossy(id),
                String::from_utf8_lossy(&chunk_id)
            ));
        }
        Ok(Cursor::new(body))
    }

    fn chunk_id(&mut self) -> Result<[u8; 4], String> {
        let mut id = [0u8; 4];
        for slot in &mut id {
            *slot = self.u8()?;
        }
        Ok(id)
    }
}

// ---------------------------------------------------------------------------
// Voice presets (GM bank built lazily once)
// ---------------------------------------------------------------------------

thread_local! {
    static MIDI_PRESETS: RefCell<Option<Rc<SiONVoicePresetUtil>>> = const { RefCell::new(None) };
}

/// GM voice presets keyed `midi.<family><n>` (e.g. `midi.pipe8` = Ocarina),
/// generated once per thread on the first MIDI playback.
fn midi_presets() -> Rc<SiONVoicePresetUtil> {
    MIDI_PRESETS.with(|slot| {
        slot.borrow_mut()
            .get_or_insert_with(|| Rc::new(SiONVoicePresetUtil::generate_voices(INCLUDE_MIDI)))
            .clone()
    })
}

/// GM program (0..127) → preset key: family = program / 8, slot 1..=8.
fn preset_key(program: u8) -> String {
    const FAMILIES: [&str; 16] = [
        "piano", "chrom", "organ", "guitar", "bass", "strings", "ensemble", "brass", "reed",
        "pipe", "lead", "pad", "fx", "percus", "world", "se",
    ];
    format!("midi.{}{}", FAMILIES[(program / 8) as usize], program % 8 + 1)
}

// ---------------------------------------------------------------------------
// Realtime player
// ---------------------------------------------------------------------------

/// The note last handed to the executor on a channel: it fires at `start`
/// (its `key_on` delay is consumed by blocks) and rings until `end`, where
/// the executor's internal note-off starts the release.
#[derive(Debug, Clone, Copy)]
struct CurrentNote {
    start: i64,
    end: i64,
    /// False until the audio clock passes `start`.
    fired: bool,
}

/// Realtime MIDI playback state: reserved tracks, per-channel GM voice,
/// the parse streams' cursors and loop bookkeeping. Created via
/// [`MidiPlayer::start`] (which reserves AFTER the driver stream started)
/// and pumped once per rendered block through [`MidiPlayer::tick`].
pub struct MidiPlayer {
    song: Rc<MidiSong>,
    /// Reserved track per channel (same order as `song.channels`).
    tracks: Vec<TrackRc>,
    /// Samples per MIDI tick at the song tempo.
    samples_per_tick: f64,
    /// Loop length in samples (`length_ticks * samples_per_tick`).
    song_length_samples: i64,
    /// Sample offset of the current loop pass (audio-clock domain).
    loop_base: i64,
    /// Next unscheduled note index per channel.
    note_cursor: Vec<usize>,
    /// Next unapplied control index per channel.
    control_cursor: Vec<usize>,
    /// The most recent note handed to the executor per channel.
    current: Vec<Option<CurrentNote>>,
    /// Latest CC7 per channel.
    volumes: Vec<u8>,
    /// Latest pitch-bend range per channel (semitones).
    bend_ranges: Vec<f32>,
}

impl MidiPlayer {
    /// Reserve one track per melodic channel and apply GM voices. Must run
    /// AFTER the driver stream started: `prepare_process` (inside
    /// `stream`/`play`) frees every track, so earlier reservations would
    /// be dropped. The player's loop clock starts at the stream's zero.
    pub fn start(driver: &mut SiONDriver, song: Rc<MidiSong>) -> MidiPlayer {
        let presets = midi_presets();
        let samples_per_tick = SAMPLE_RATE as f64 * 60.0 / (song.bpm * song.ppq as f64);

        let mut tracks = Vec::with_capacity(song.channels.len());
        for (slot, channel) in song.channels.iter().enumerate() {
            // Slot-ordered ids keep the reserved set disjoint from the
            // driver-triggered (disposable) id space.
            let track = driver
                .create_user_controllable_track(slot as i32)
                .unwrap_or_else(|| panic!("MidiPlayer: cannot reserve track {slot}"));
            // Preset voice, with a plain default voice as fallback (sine-ish
            // FM default when a GM preset is missing from the bank). Presets
            // are `Rc<RefCell<SiONVoice>>` and SiONVoice is not Clone, so the
            // voice updates the track straight through a temporary borrow.
            let preset = presets.get_voice_preset(&preset_key(channel.program));
            {
                let mut tb = track.borrow_mut();
                let chip = driver.chip.clone();
                match &preset {
                    Some(rc) => rc.borrow().update_track_voice(&mut tb, &mut *chip.borrow_mut()),
                    None => {
                        SiONVoice::default_voice().update_track_voice(&mut tb, &mut *chip.borrow_mut())
                    }
                }
                tb.set_velocity(0);
                tb.set_expression(128);
                tb.set_pitch_bend(0);
                tb.set_master_volume(0); // gain applied per block in `tick`
            }
            tracks.push(track);
        }

        let channel_total = song.channels.len();
        MidiPlayer {
            song_length_samples: (song.length_ticks as f64 * samples_per_tick).round() as i64,
            song,
            tracks,
            samples_per_tick,
            loop_base: 0,
            note_cursor: vec![0; channel_total],
            control_cursor: vec![0; channel_total],
            current: vec![None; channel_total],
            volumes: vec![DEFAULT_VOLUME; channel_total],
            bend_ranges: vec![DEFAULT_BEND_RANGE; channel_total],
        }
    }

    /// Sample position of one tick event in the current loop pass.
    fn event_sample(&self, tick: u32) -> i64 {
        self.loop_base + (tick as f64 * self.samples_per_tick).round() as i64
    }

    /// Advance everything by one rendered block. `gain` is the music-bus
    /// level (Options slider); CC7 master volumes ride on top of it.
    pub fn tick(&mut self, driver: &mut SiONDriver, gain: f32) {
        let now = driver.sequencer.borrow().get_processed_sample_count() as i64;
        let block = driver.get_buffer_length() as i64;

        // Loop wrap: restart the cursors; bend returns to center so a
        // loop-end bend cannot detune the next pass; cut ringing tails so
        // envelopes release instead of droning across the seam.
        if now >= self.loop_base + self.song_length_samples {
            self.loop_base += self.song_length_samples;
            for cursor in &mut self.note_cursor {
                *cursor = 0;
            }
            for cursor in &mut self.control_cursor {
                *cursor = 0;
            }
            for current in &mut self.current {
                *current = None;
            }
            for range in &mut self.bend_ranges {
                *range = DEFAULT_BEND_RANGE;
            }
            for track in &self.tracks {
                let chip = driver.chip.clone();
                {
                    let mut tb = track.borrow_mut();
                    tb.set_pitch_bend(0);
                    tb.set_expression(128);
                    tb.set_velocity(0);
                }
                track.borrow_mut().key_off(0, false, &mut *chip.borrow_mut());
            }
        }

        for (slot, channel) in self.song.channels.iter().enumerate() {
            let Some(track) = self.tracks.get(slot) else { continue };

            // --- realtime controls (block granularity, never early) ----
            let mut bend14: Option<i32> = None;
            let mut expression: Option<u8> = None;
            let mut index = self.control_cursor[slot];
            while let Some(event) = channel.controls.get(index) {
                if self.event_sample(event.tick) > now {
                    break;
                }
                match event.control {
                    MidiControl::Bend(v) => bend14 = Some(v),
                    MidiControl::Expression(v) => expression = Some(v),
                    MidiControl::Volume(v) => self.volumes[slot] = v,
                    MidiControl::BendRange(r) => self.bend_ranges[slot] = r.max(0.0),
                }
                index += 1;
            }
            self.control_cursor[slot] = index;

            {
                let mut tb = track.borrow_mut();
                if let Some(v) = bend14 {
                    // 64 track units = 1 semitone (MML pitch units).
                    let semis = (v - 8192) as f32 * self.bend_ranges[slot] / 8192.0;
                    tb.set_pitch_bend((semis * 64.0).round() as i32);
                }
                if let Some(v) = expression {
                    tb.set_expression(v as i32);
                }
                let master = (self.volumes[slot] as f32 / 127.0 * VOLUME_SPAN * gain)
                    .round()
                    .clamp(0.0, VOLUME_SPAN);
                tb.set_master_volume(master as i32);
            }

            // --- note scheduling --------------------------------------
            // Retire the current note once it fired; the executor owns the
            // note-off from there (key_on_counter → internal release).
            if let Some(current) = &mut self.current[slot] {
                if now >= current.start {
                    current.fired = true;
                }
            }
            let current = self.current[slot]; // Copy
            if current.is_some_and(|c| !c.fired) {
                continue; // executor slot busy with an un-fired note
            }

            // Arm the next note at the last pump before its start (the
            // one-block window keeps the arming delay from freezing a
            // ringing note longer than one block). Gap-following notes
            // wait for the previous note-off so its release rings out.
            while let Some(next) = channel.notes.get(self.note_cursor[slot]) {
                let start = self.event_sample(next.start_tick);
                if start > now + block {
                    break; // beyond the next rendered block
                }
                if let Some(prev) = current {
                    if start > prev.end && now < prev.end {
                        break; // gap: let the previous release ring out first
                    }
                }
                let length_res = ((next.end_tick - next.start_tick) as f64 * RESOLUTION
                    / self.song.ppq as f64)
                    .round();
                let delay = (start - now).max(0) as i32;
                {
                    let mut tb = track.borrow_mut();
                    tb.set_velocity(
                        (next.velocity as f32 * VELOCITY_SCALE).round().clamp(0.0, 512.0) as i32,
                    );
                    tb.key_on(next.note, length_res as i32, delay);
                }
                self.current[slot] = Some(CurrentNote {
                    start,
                    end: self.event_sample(next.end_tick),
                    fired: false,
                });
                self.note_cursor[slot] += 1;
                break;
            }
        }
    }

    /// Number of reserved tracks (one per melodic channel).
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }
}