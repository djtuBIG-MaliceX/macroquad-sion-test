//! `.mid` bus contract: the SMF parser turns files into per-channel note +
//! control streams, playback reserves one `USER_CONTROLLED` track per melodic
//! channel (after `stream(true)`, which frees every track), and the same-path
//! music guard keeps an already-running track playing — screen switches must
//! never restart it, only an actual track change may.

use std::fs;
use std::path::Path;

use macroquad_sion_test::audio::midi::{self, MidiControl, MidiSong};
use macroquad_sion_test::audio::{AudioEngine, SfxId};

const MIDI_DEMO: &str = "assets/voice/furina_meets_link.mid";
const TITLE_THEME_PATH: &str = macroquad_sion_test::config::TITLE_THEME_PATH;

fn read_bytes(rel: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn read(rel: &str) -> String {
    String::from_utf8(read_bytes(rel)).expect("mml assets are utf-8")
}

// ---------------------------------------------------------------------------
// SMF byte builder (hand-rolled, mirrors the parser's expectations)
// ---------------------------------------------------------------------------

/// Variable-length quantity (delta times, chunk lengths, meta payloads).
fn vlq(mut value: u32) -> Vec<u8> {
    let mut out = vec![(value & 0x7F) as u8];
    value >>= 7;
    while value > 0 {
        out.insert(0, ((value & 0x7F) as u8) | 0x80);
        value >>= 7;
    }
    out
}

fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// Assemble one file: `MThd` (format / ntrks / division) + `MTrk` chunks.
fn smf(format: u16, division: u16, tracks: &[Vec<u8>]) -> Vec<u8> {
    let mut header = Vec::new();
    header.extend_from_slice(&format.to_be_bytes());
    header.extend_from_slice(&(tracks.len() as u16).to_be_bytes());
    header.extend_from_slice(&division.to_be_bytes());

    let mut out = chunk(b"MThd", &header);
    for track in tracks {
        out.extend_from_slice(&chunk(b"MTrk", track));
    }
    out
}

/// One track body: `(delta, event bytes)` pairs + the `FF 2F 00` end marker.
fn track(events: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (delta, event) in events {
        body.extend_from_slice(&vlq(*delta));
        body.extend_from_slice(event);
    }
    body.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);
    body
}

fn tempo(us_per_quarter: u32) -> Vec<u8> {
    vec![
        0xFF,
        0x51,
        0x03,
        (us_per_quarter >> 16) as u8,
        (us_per_quarter >> 8) as u8,
        us_per_quarter as u8,
    ]
}

fn note_on(channel: u8, key: u8, velocity: u8) -> Vec<u8> {
    vec![0x90 | channel, key, velocity]
}

fn note_off(channel: u8, key: u8) -> Vec<u8> {
    vec![0x80 | channel, key, 0x40]
}

fn program(channel: u8, program: u8) -> Vec<u8> {
    vec![0xC0 | channel, program]
}

fn cc(channel: u8, controller: u8, value: u8) -> Vec<u8> {
    vec![0xB0 | channel, controller, value]
}

fn bend(channel: u8, value14: i32) -> Vec<u8> {
    vec![0xE0 | channel, (value14 & 0x7F) as u8, (value14 >> 7) as u8]
}

/// Minimal one-note song: ch0 / program 0 / key 60, `ppq` ticks per quarter.
fn tiny_midi(ppq: u16, note_ticks: u32) -> Vec<u8> {
    smf(
        0,
        ppq,
        &[track(&[
            (0, tempo(500_000)),
            (0, program(0, 0)),
            (0, note_on(0, 60, 100)),
            (note_ticks, note_off(0, 60)),
        ])],
    )
}

/// One sustained ocarina note (key 67, 360 ticks = 1.5 s at PPQ 120 / 120 BPM)
/// under fixed CC7 and CC11, for control-scaling measurements.
fn sustained(cc7: u8, cc11: u8, velocity: u8) -> Vec<u8> {
    smf(
        0,
        120,
        &[track(&[
            (0, tempo(500_000)),
            (0, program(0, 79)),
            (0, cc(0, 7, cc7)),
            (0, cc(0, 11, cc11)),
            (0, note_on(0, 67, velocity)),
            (360, note_off(0, 67)),
        ])],
    )
}

/// A *cue*, not a song: `channels` channels in lockstep, each playing `notes`
/// short notes one per slot (60 ticks = 0.25 s at PPQ 120 / 120 BPM), so the
/// file ends by itself after `(notes - 1) * 60 + 30` ticks.
fn cue_midi(channels: usize, notes: u32) -> Vec<u8> {
    let key = |ch: u8, i: u32| 60 + ch * 4 + (i % 3) as u8;
    let mut events: Vec<(u32, Vec<u8>)> = vec![(0, tempo(500_000))];
    for ch in 0..channels as u8 {
        events.push((0, program(ch, 79)));
        events.push((0, cc(ch, 7, 100)));
    }
    for i in 0..notes {
        // All channels share one slot: the advance rides the first event of
        // each column, so interleaving stays sample-aligned per channel.
        for (idx, ch) in (0..channels as u8).enumerate() {
            let delta = if idx == 0 { if i == 0 { 0 } else { 30 } } else { 0 };
            events.push((delta, note_on(ch, key(ch, i), 96)));
        }
        for idx in 0..channels as u8 {
            events.push((if idx == 0 { 30 } else { 0 }, note_off(idx, key(idx, i))));
        }
    }
    smf(0, 120, &[track(&events)])
}

/// Loudest rendered sample between two times (seconds), both channels.
fn peak_between(buf: &[f32], from_s: f64, to_s: f64) -> f64 {
    let sr = 44100.0 * 2.0; // stereo-interleaved
    let a = (from_s * sr) as usize;
    let b = ((to_s * sr) as usize).min(buf.len());
    buf[a..b].iter().map(|s| s.abs() as f64).fold(0.0f64, f64::max)
}

/// Peak amplitude of a MIDI file played as music, measured over `window`.
fn heard_as_music(bytes: &[u8], window: (f64, f64)) -> f64 {
    let mut engine = AudioEngine::new(&[]);
    engine.add_midi_song("measure.mid", bytes);
    engine.play_music("measure.mid");
    let blocks = (window.1 * 44100.0 / 2048.0).ceil() as usize + 1;
    let buf = engine.pump_capture(blocks);
    drop(engine);
    peak_between(&buf, window.0, window.1)
}

// ---------------------------------------------------------------------------
// parser
// ---------------------------------------------------------------------------

#[test]
fn midi_paths_route_by_extension() {
    assert!(midi::is_midi_path("assets/voice/furina_meets_link.mid"));
    assert!(midi::is_midi_path("assets/voice/x.midi"));
    assert!(midi::is_midi_path("assets/voice/x.MID"));
    assert!(!midi::is_midi_path("assets/audio/options_theme.mml"));
    assert!(!midi::is_midi_path("assets/sfx/voice_cue.ogg"));
    assert!(!midi::is_midi_path("mid"));
    assert!(!midi::is_midi_path(""));
}

#[test]
fn format0_parses_tempo_ppq_and_sorted_streams() {
    // Two channels interleaved in one (format 0) track, PPQ 96, 120 BPM.
    let file = smf(
        0,
        96,
        &[track(&[
            (0, tempo(500_000)),
            (0, program(0, 79)),
            (0, cc(0, 7, 100)),
            (24, note_on(0, 60, 80)),
            (0, cc(1, 11, 40)),
            (0, note_on(1, 55, 100)),
            (48, note_off(0, 60)), // t72
            (24, note_off(1, 55)), // t96
            (0, note_on(0, 62, 64)),
            (96, note_off(0, 62)), // t192
        ])],
    );

    let song = MidiSong::parse(&file).expect("valid format-0 file");
    assert_eq!(song.bpm, 120.0, "500_000 us per quarter = 120 BPM");
    assert_eq!(song.ppq, 96);
    assert_eq!(song.length_ticks, 192, "last note-off bounds the loop");

    assert_eq!(song.channels.len(), 2, "note-less channels are dropped, not these");
    let ch0 = &song.channels[0];
    let ch1 = &song.channels[1];
    assert_eq!((ch0.channel, ch0.program), (0, 79));

    let notes: Vec<(i32, u8, u32, u32)> =
        ch0.notes.iter().map(|n| (n.note, n.velocity, n.start_tick, n.end_tick)).collect();
    assert_eq!(notes, [(60, 80, 24, 72), (62, 64, 96, 192)], "ch0 notes sorted by start");
    let notes_ch1: Vec<(i32, u32, u32)> =
        ch1.notes.iter().map(|n| (n.note, n.start_tick, n.end_tick)).collect();
    assert_eq!(notes_ch1, [(55, 24, 96)]);

    assert!(ch0
        .controls
        .iter()
        .any(|e| e.tick == 0 && e.control == MidiControl::Volume(100)));
    assert!(ch1
        .controls
        .iter()
        .any(|e| e.tick == 24 && e.control == MidiControl::Expression(40)));
}

#[test]
fn rpn_bend_range_is_ours_nrpn_data_entry_is_ignored() {
    let file = smf(
        0,
        96,
        &[track(&[
            (0, note_on(0, 60, 90)),
            // Same tick, bend arriving BEFORE the RPN negotiation lands:
            // rank ordering must still put the range change first.
            (10, bend(0, 10000)),
            (0, cc(0, 0x65, 0)),
            (0, cc(0, 0x64, 0)),
            (0, cc(0, 6, 24)), // data entry -> bend range 24
            (10, cc(0, 0x63, 1)), // NRPN selection: data entry below is not ours
            (0, cc(0, 0x62, 2)),
            (0, cc(0, 6, 64)), // ignored
            (10, cc(0, 0x65, 1)), // foreign RPN MSB forces NotOurs
            (0, cc(0, 6, 12)), // ignored
            (10, cc(0, 0x64, 7)), // nonzero RPN LSB -> NotOurs
            (0, cc(0, 6, 5)),  // ignored
            (0, bend(0, 8192)),
            (10, note_off(0, 60)),
        ])],
    );

    let song = MidiSong::parse(&file).expect("valid");
    let controls: Vec<(u32, MidiControl)> =
        song.channels[0].controls.iter().map(|e| (e.tick, e.control)).collect();
    assert_eq!(
        controls,
        [
            (10, MidiControl::BendRange(24.0)),
            (10, MidiControl::Bend(10000)),
            (40, MidiControl::Bend(8192)),
        ],
        "only RPN(0,0) data entry lands; NRPN/foreign-range CC6 dropped; range sorts before bend"
    );
}

#[test]
fn notes_pair_fifo_orphan_offs_drop_and_open_ons_clip_at_end() {
    let file = smf(
        0,
        96,
        &[track(&[
            (0, note_on(0, 60, 70)),  // t0
            (5, note_on(1, 40, 90)),  // t5  (never closed -> end clip)
            (5, note_on(0, 60, 80)),  // t10 (second on same key)
            (5, note_off(0, 60)),     // t15 pairs FIFO with the t0 on
            (5, note_off(0, 61)),     // t20 orphan: no matching on
            (10, note_off(0, 60)),    // t30 pairs with the t10 on
            (10, cc(1, 7, 64)),       // t40 keeps the track end here
        ])],
    );

    let song = MidiSong::parse(&file).expect("valid");
    assert_eq!(song.length_ticks, 40, "track end includes the trailing CC");

    let ch0_notes: Vec<(i32, u8, u32, u32)> =
        song.channels[0].notes.iter().map(|n| (n.note, n.velocity, n.start_tick, n.end_tick)).collect();
    assert_eq!(ch0_notes, [(60, 70, 0, 15), (60, 80, 10, 30)], "FIFO pairing per key");
    let ch1_notes: Vec<(i32, u8, u32, u32)> =
        song.channels[1].notes.iter().map(|n| (n.note, n.velocity, n.start_tick, n.end_tick)).collect();
    assert_eq!(ch1_notes, [(40, 90, 5, 40)], "unclosed note clips at the track end");
}

#[test]
fn drums_and_noteless_channels_never_reach_playback() {
    // Channel 9 (GM drums) + a note-less channel are parsed then dropped.
    let drums_only = smf(
        0,
        96,
        &[track(&[
            (0, note_on(9, 36, 100)),
            (24, note_off(9, 36)),
        ])],
    );
    let err = MidiSong::parse(&drums_only).unwrap_err();
    assert!(err.contains("no melodic notes"), "drums alone: {err}");

    let noteless = smf(
        0,
        96,
        &[track(&[
            (0, program(1, 40)),
            (48, cc(1, 7, 90)),
        ])],
    );
    let err = MidiSong::parse(&noteless).unwrap_err();
    assert!(err.contains("no melodic notes"), "controls only: {err}");
}

#[test]
fn malformed_containers_reject_without_panicking() {
    // SMPTE division (high bit set).
    let smpte = smf(0, 0xE250, &[track(&[(0, note_on(0, 60, 1)), (1, note_off(0, 60))])]);
    let err = MidiSong::parse(&smpte).unwrap_err();
    assert!(err.contains("SMPTE"), "{err}");

    // Format 2 (sequential tracks) is out of scope.
    let fmt2 = smf(2, 96, &[track(&[(0, note_on(0, 60, 1)), (1, note_off(0, 60))])]);
    let err = MidiSong::parse(&fmt2).unwrap_err();
    assert!(err.contains("format 2"), "{err}");

    // Zero PPQ.
    let zero = smf(0, 0, &[track(&[(0, note_on(0, 60, 1))])]);
    let err = MidiSong::parse(&zero).unwrap_err();
    assert!(err.contains("PPQ"), "{err}");

    // Wrong leading chunk id.
    let mut wrong = chunk(b"MTrk", &[0x00]);
    wrong.extend_from_slice(&smf(0, 96, &[])[6..]);
    let err = MidiSong::parse(&wrong).unwrap_err();
    assert!(err.contains("expected chunk"), "{err}");

    // Truncated payload.
    let truncated = &smf(0, 96, &[track(&[(0, note_on(0, 60, 1))])])[..14];
    assert!(MidiSong::parse(truncated).is_err());
}

#[test]
fn demo_asset_parses_with_rpn_range_gm_voices_and_loops() {
    let bytes = read_bytes(MIDI_DEMO);
    let song = MidiSong::parse(&bytes).expect("shipped demo asset must parse");

    assert_eq!(song.ppq, 120);
    assert_eq!(song.bpm, 120.0);
    assert_eq!(song.length_ticks, 5280, "loop length = last used tick");
    assert!(!song.channels.is_empty());

    // Every melodic channel is the same ocarina GM program (79 -> midi.pipe8)
    // and negotiated its ±24-semitone bend range through RPN (0,0).
    for channel in &song.channels {
        assert_eq!(channel.program, 79, "channel {} program", channel.channel);
        assert!(
            channel.controls.iter().any(|e| e.control == MidiControl::BendRange(24.0)),
            "channel {} misses the RPN 24-semitone range",
            channel.channel
        );
        assert!(channel
            .controls
            .iter()
            .any(|e| matches!(e.control, MidiControl::Bend(v) if v != 8192)),
            "channel {} uses pitch bend expressively", channel.channel);
        assert!(channel
            .controls
            .iter()
            .any(|e| matches!(e.control, MidiControl::Expression(_))),
            "channel {} rides CC11 expression", channel.channel);
    }
    assert!(song
        .channels
        .iter()
        .any(|c| c.controls.iter().any(|e| e.control == MidiControl::Volume(100))),
        "some channel sets CC7 volume");
}

#[test]
fn understated_mtrk_lengths_recover_to_the_next_header() {
    // Mirrors the demo asset: the MTrk declares a length far shorter than its
    // real body. The parser scans up to the next chunk header and stops at
    // `FF 2F` as usual, then resumes chunk reading exactly at the next track.
    let mut broken = Vec::new();
    for (delta, event) in [
        (0u32, tempo(500_000)),
        (0, program(0, 40)),
        (0, note_on(0, 60, 100)),
        (24, note_off(0, 60)),
    ] {
        broken.extend_from_slice(&vlq(delta));
        broken.extend_from_slice(&event);
    }
    broken.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);

    let track2 = track(&[(0, program(1, 0)), (12, note_on(1, 67, 90)), (24, note_off(1, 67))]);

    let mut file = Vec::new();
    file.extend_from_slice(b"MThd");
    file.extend_from_slice(&6u32.to_be_bytes());
    file.extend_from_slice(&1u16.to_be_bytes()); // format 1
    file.extend_from_slice(&2u16.to_be_bytes()); // ntrks
    file.extend_from_slice(&96u16.to_be_bytes()); // ppq
    file.extend_from_slice(b"MTrk");
    file.extend_from_slice(&6u32.to_be_bytes()); // UNDERSTATED (real body: 23)
    file.extend_from_slice(&broken);
    file.extend_from_slice(&chunk(b"MTrk", &track2));

    let song = MidiSong::parse(&file).expect("understated MTrk lengths must recover");

    assert_eq!(song.bpm, 120.0);
    assert_eq!(song.ppq, 96);
    assert_eq!(song.channels.len(), 2, "both tracks survived the broken length");
    let (ch0, ch1) = (&song.channels[0], &song.channels[1]);
    assert_eq!((ch0.channel, ch0.program), (0, 40));
    assert_eq!(
        ch0.notes.iter().map(|n| (n.start_tick, n.end_tick)).collect::<Vec<_>>(),
        [(0, 24)]
    );
    assert_eq!((ch1.channel, ch1.program), (1, 0));
    assert_eq!(
        ch1.notes.iter().map(|n| (n.start_tick, n.end_tick)).collect::<Vec<_>>(),
        [(12, 36)]
    );
    assert_eq!(song.length_ticks, 36);
}

#[test]
fn running_status_survives_meta_and_dies_at_sysex() {
    // Real-world writers omit the status byte after meta events; sysex ends
    // the running status (an explicit status must follow).
    let body = track(&[
        (0, tempo(500_000)),
        (0, note_on(0, 60, 100)),                 // explicit: running = 0x90
        (12, vec![0xFF, 0x01, 0x02, b'a', b'b']), // text meta keeps running status
        (12, vec![60, 0]),                        // data-only note-off via running status
        (0, vec![0xF0, 0x03, 0x7E, 0x00, 0xF7]),  // sysex clears it
        (12, note_on(0, 64, 90)),                 // explicit again
        (12, vec![64, 0]),                        // running note-off again
    ]);
    let song = MidiSong::parse(&smf(0, 96, &[body])).expect("running status across meta");

    assert_eq!(song.bpm, 120.0);
    let notes: Vec<(i32, u32, u32)> =
        song.channels[0].notes.iter().map(|n| (n.note, n.start_tick, n.end_tick)).collect();
    assert_eq!(notes, [(60, 0, 24), (64, 36, 48)], "meta preserved 0x90, sysex reset it");

    // Data bytes after sysex without a status byte are an error, not garbage.
    let broken = track(&[
        (0, note_on(0, 60, 100)),
        (12, vec![0xF0, 0x03, 0x7E, 0x00, 0xF7]),
        (12, vec![60, 0]),
    ]);
    let err = MidiSong::parse(&smf(0, 96, &[broken])).unwrap_err();
    assert!(err.contains("data byte without status"), "{err}");
}

// ---------------------------------------------------------------------------
// playback on the engine
// ---------------------------------------------------------------------------

#[test]
fn playback_reserves_a_track_per_channel_and_never_restarts_on_same_path() {
    let bytes = read_bytes(MIDI_DEMO);
    let parsed = MidiSong::parse(&bytes).unwrap();
    let mut engine = AudioEngine::new(&[]);

    engine.add_midi_song(MIDI_DEMO, &bytes);
    assert!(engine.has_track(MIDI_DEMO));

    engine.play_music(MIDI_DEMO);
    assert!(engine.is_streaming(), "MIDI streams the driver");
    assert_eq!(engine.current_music().as_deref(), Some(MIDI_DEMO));
    assert_eq!(
        engine.midi_track_count(),
        parsed.channels.len(),
        "one reserved USER_CONTROLLED track per melodic channel"
    );
    assert!(
        engine.music_track_master_volumes().is_empty(),
        "MIDI tracks are not MML music tracks (separate gain path)"
    );

    engine.pump_for_test(4);
    let clock = engine.processed_sample_count();
    assert!(clock > 0, "engine pumps while MIDI plays");

    // Screen-switch replay of the SAME track: the guard keeps it running.
    engine.play_music(MIDI_DEMO);
    engine.pump_for_test(2);
    assert!(
        engine.processed_sample_count() > clock,
        "same-path replay must continue the stream, not restart it"
    );
    assert_eq!(engine.midi_track_count(), parsed.channels.len(), "no re-reservation");
    assert_eq!(engine.current_music().as_deref(), Some(MIDI_DEMO));
}

#[test]
fn switching_midi_to_mml_reclaims_the_stage() {
    let bytes = read_bytes(MIDI_DEMO);
    let mml = read("assets/audio/title_theme.mml");
    let mut engine = AudioEngine::new(&[]);

    engine.add_midi_song(MIDI_DEMO, &bytes);
    engine.play_music(MIDI_DEMO);
    engine.pump_for_test(2);
    assert!(engine.midi_track_count() > 0);

    engine.add_song(TITLE_THEME_PATH, mml);
    engine.play_music(TITLE_THEME_PATH);
    assert_eq!(engine.midi_track_count(), 0, "the old player drops synchronously");
    engine.pump_for_test(3);
    assert!(!engine.music_track_master_volumes().is_empty(), "MML tracks materialise");
    assert!(engine.is_streaming());
}

#[test]
fn mid_session_midi_registration_leaves_the_mml_stream_untouched() {
    let mml = read("assets/audio/title_theme.mml");
    let mut engine = AudioEngine::new(&[]);

    engine.add_song(TITLE_THEME_PATH, mml);
    engine.set_music_volume(0.5);
    engine.start_music();
    engine.pump_for_test(3);
    let volumes = engine.music_track_master_volumes();
    assert!(!volumes.is_empty());

    // Parse-only registration must not compile() (which would stop the
    // stream and free the playing tracks).
    let bytes = tiny_midi(96, 48);
    engine.add_midi_song("assets/voice/extra.mid", &bytes);
    assert_eq!(engine.music_track_master_volumes(), volumes, "registration disturbed playback");
    assert!(engine.is_streaming());

    // ...and the new song is immediately playable.
    engine.play_music("assets/voice/extra.mid");
    assert_eq!(engine.midi_track_count(), 1, "one note channel -> one reserved track");
    assert_eq!(engine.current_music().as_deref(), Some("assets/voice/extra.mid"));
}

#[test]
fn same_path_guard_holds_for_mml_music_too() {
    let mml = read("assets/audio/title_theme.mml");
    let mut engine = AudioEngine::new(&[]);
    engine.add_song(TITLE_THEME_PATH, mml);

    engine.start_music();
    engine.pump_for_test(3);
    let clock = engine.processed_sample_count();
    assert!(clock > 0);

    // Re-entering the screen replays its theme: must keep playing.
    engine.start_music();
    engine.pump_for_test(1);
    assert!(
        engine.processed_sample_count() > clock,
        "same-path MML replay must continue the stream, not restart it"
    );
}

#[test]
fn short_midi_loops_indefinitely_without_leaking_tracks() {
    let bytes = tiny_midi(96, 48);
    let mut engine = AudioEngine::new(&[]);

    engine.add_midi_song("assets/voice/loop_test.mid", &bytes);
    engine.play_music("assets/voice/loop_test.mid");
    // ~11k samples per pass vs 2048-sample blocks: many loop wraps happen.
    engine.pump_for_test(40);

    assert!(engine.is_streaming(), "auto-stop stays off across the loop seam");
    assert_eq!(engine.midi_track_count(), 1, "loop wraps never re-reserve");
    assert_eq!(engine.current_music().as_deref(), Some("assets/voice/loop_test.mid"));
}

#[test]
fn unparseable_midi_registration_stays_silent_not_fatal() {
    let mut engine = AudioEngine::new(&[]);
    engine.add_midi_song("assets/voice/broken.mid", b"not a midi file");
    assert!(!engine.has_track("assets/voice/broken.mid"), "failed parse is not registered");

    engine.play_music("assets/voice/broken.mid");
    assert_eq!(engine.current_music(), None, "unknown track -> silence, no panic");
    assert!(!engine.is_streaming());

    // The engine still pumps (file-SFX-only sessions share this path).
    let sfx: Vec<(SfxId, String)> = SfxId::ALL
        .iter()
        .map(|(id, _stem)| (*id, read(&id.asset_path()).trim().to_string()))
        .collect();
    drop(engine);
    let mut engine = AudioEngine::new(&sfx);
    engine.pump_for_test(2);
}

// ---------------------------------------------------------------------------
// expression + volume scaling (measured through the render)
// ---------------------------------------------------------------------------

#[test]
fn cc11_expression_scales_its_channels_cc7_volume() {
    // MIDI expression is a linear amplitude scale: CC11 64 under the same CC7
    // must land near half the level, and CC11 0 must be silent. Measured on a
    // sustained note so the number is the channel level, not an attack.
    let window = (0.35, 1.3);
    let full = heard_as_music(&sustained(100, 127, 100), window);
    assert!(full > 0.05, "the note sounds at all (peak {full})");

    let half = heard_as_music(&sustained(100, 64, 100), window) / full;
    assert!((0.44..=0.58).contains(&half), "CC11 64 measured {half:.3}, want ~0.5");

    let weak = heard_as_music(&sustained(100, 16, 100), window) / full;
    assert!((0.08..=0.20).contains(&weak), "CC11 16 measured {weak:.3}, want ~0.13");

    let mute = heard_as_music(&sustained(100, 0, 100), window);
    assert!(mute < full * 0.05, "CC11 0 is silence (peak {mute})");
}

#[test]
fn expression_keeps_its_range_at_maximum_velocity() {
    // Regression guard for the velocity mapping: SiON's velocity table is only
    // linear up to 256, and a MIDI-strong note mapped above it used to sit in
    // the clamped "distortion" region where the total-level ceiling swallowed
    // every CC11 swing (measured 64 -> 0.78 of full instead of ~0.5).
    let window = (0.35, 1.3);
    let full = heard_as_music(&sustained(100, 127, 127), window);
    let half = heard_as_music(&sustained(100, 64, 127), window) / full;
    assert!((0.44..=0.58).contains(&half), "CC11 64 at max velocity: {half:.3}");
    let weak = heard_as_music(&sustained(100, 32, 127), window) / full;
    assert!((0.18..=0.32).contains(&weak), "CC11 32 at max velocity: {weak:.3}");
}

// ---------------------------------------------------------------------------
// .mid as a sound effect, over continuing music
// ---------------------------------------------------------------------------

#[test]
fn the_midi_bus_leaves_headroom_like_a_hand_authored_song() {
    // Eight GM channels are eight full-level voices on their own tracks: with
    // velocities and bus gain taken at face value the sum pins the master
    // (measured 48 of 220 blocks hard-clipped before the scaling existed).
    // Assert the shipped file stays off the ceiling and lands in the loudness
    // band of the hand-authored themes, so expression has room to swell into.
    let mut engine = AudioEngine::new(&[]);
    engine.add_midi_song(MIDI_DEMO, &read_bytes(MIDI_DEMO));
    engine.play_music(MIDI_DEMO);
    let buf = engine.pump_capture(150); // ~7 s of the densest opening
    drop(engine);

    let full_scale = buf.iter().filter(|s| s.abs() >= 0.99).count();
    assert_eq!(full_scale, 0, "no sample anywhere near full scale");
    let peak = buf.iter().map(|s| s.abs() as f64).fold(0.0f64, f64::max);
    assert!(peak < 0.85, "peaks keep headroom: {peak:.3}");
    let rms = (buf.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / buf.len() as f64).sqrt();
    assert!((0.05..=0.25).contains(&rms), "bus loudness band, measured {rms:.4}");
}

const CUE: &str = "assets/voice/cue.mid";

#[test]
fn midi_cue_plays_over_the_music_and_hands_its_tracks_back() {
    let mml = read("assets/audio/title_theme.mml");
    let mut engine = AudioEngine::new(&[]);
    engine.add_song(TITLE_THEME_PATH, mml);
    engine.set_music_volume(0.5);
    engine.start_music();
    engine.pump_for_test(3);
    let music_volumes = engine.music_track_master_volumes();
    assert!(!music_volumes.is_empty(), "theme is sounding");

    // Preloaded like any other `sfx:` reference, then fired.
    engine.add_midi_song(CUE, &cue_midi(2, 3));
    engine.play_file_sfx(CUE, 0.9);

    assert_eq!(
        engine.current_music().as_deref(),
        Some(TITLE_THEME_PATH),
        "the cue never claims the music bus"
    );
    assert!(engine.is_streaming());
    assert_eq!(engine.midi_track_count(), 0, "no music-bus MIDI player appeared");
    assert_eq!(engine.midi_effect_track_count(), 2, "one reserved track per channel");
    assert_eq!(engine.midi_effect_paths(), vec![CUE.to_string()]);

    engine.pump_for_test(6);
    assert_eq!(engine.current_music().as_deref(), Some(TITLE_THEME_PATH));
    assert_eq!(
        engine.music_track_master_volumes(),
        music_volumes,
        "music baseline gain untouched by the cue"
    );

    // Past the end of the cue (150 ticks = 0.625 s) plus release tails:
    // silent again, and its reservations returned to the pool.
    engine.pump_for_test(30);
    assert_eq!(engine.midi_effect_track_count(), 0, "finished cue released its tracks");
    assert!(engine.midi_effect_paths().is_empty());
    assert!(engine.is_streaming(), "the music it played over still streams");

    // Re-firing the same cue reuses those slots instead of asking for more: a
    // cue triggered on every scene entry must not keep growing the pool. (The
    // pool is measured after one full pass, because a released track stays in
    // the sequencer's list until the next reservation picks it up.)
    let pool_after_first = engine.total_track_count();
    for _ in 0..5 {
        engine.play_file_sfx(CUE, 0.9);
        assert_eq!(engine.midi_effect_track_count(), 2, "cue re-reserved");
        engine.pump_for_test(36);
    }
    assert_eq!(engine.midi_effect_track_count(), 0, "last pass finished");
    assert_eq!(
        engine.total_track_count(),
        pool_after_first,
        "repeated cues recycle the same pool (no leaked USER_CONTROLLED slots)"
    );
}

#[test]
fn midi_cue_without_music_borrows_the_stream_and_gives_it_back() {
    let mut engine = AudioEngine::new(&[]);
    engine.add_midi_song(CUE, &cue_midi(1, 2));

    engine.play_file_sfx(CUE, 1.0);
    assert!(engine.is_streaming(), "a cue with an idle driver starts its own stream");
    assert_eq!(engine.midi_effect_track_count(), 1);

    engine.pump_for_test(30);
    assert_eq!(engine.midi_effect_track_count(), 0, "cue done");
    assert!(!engine.is_streaming(), "and it closed the stream it borrowed");

    // A cue fired after that gets a working stage again.
    engine.play_file_sfx(CUE, 1.0);
    assert!(engine.is_streaming());
    engine.pump_for_test(30);
    assert!(!engine.is_streaming());
}

#[test]
fn a_fourth_simultaneous_cue_cuts_the_oldest() {
    let mut engine = AudioEngine::new(&[]);
    let bytes = cue_midi(1, 40); // long enough to still be sounding
    for i in 0..4 {
        let path = format!("assets/voice/cue{i}.mid");
        engine.add_midi_song(&path, &bytes);
        engine.play_file_sfx(&path, 1.0);
    }
    let live = engine.midi_effect_paths();
    assert_eq!(
        live,
        vec!["assets/voice/cue1.mid", "assets/voice/cue2.mid", "assets/voice/cue3.mid"],
        "oldest cue was cut to cap polyphony"
    );
    engine.pump_for_test(4);
    assert_eq!(engine.midi_effect_track_count(), 3, "one channel per surviving cue");
}
