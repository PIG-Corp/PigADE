use super::*;
use pigade_storage::{EventKind, Journal, Receipt};
use pigade_terminal::{CORE_PROFILE, Geometry, Terminal};
use uuid::Uuid;

fn source() -> JournalHeader {
    JournalHeader {
        session_id: Uuid::new_v4(),
        generation: Uuid::new_v4(),
        profile: CORE_PROFILE.into(),
        initial_rows: 8,
        initial_cols: 40,
    }
}
fn apply(
    journal: &mut Journal,
    terminal: &mut Terminal,
    history: &History,
    kind: EventKind,
    bytes: &[u8],
) -> Result<Receipt> {
    let receipt = journal.append(kind, bytes)?;
    history.begin(receipt.sequence)?;
    match kind {
        EventKind::Output => terminal.output(bytes),
        EventKind::Resize => terminal.resize(serde_json::from_slice(bytes)?),
        EventKind::SyncTimeout => terminal.sync_timeout(),
        EventKind::Exit => (),
    }
    history.commit()?;
    Ok(receipt)
}

fn reassemble_chunks(history: &History, sequence: u64, ordinal: u64) -> Result<Vec<u8>> {
    let mut offset = 0;
    let mut record = Vec::new();
    let mut expected_size = None;
    let mut source_sequence = None;
    loop {
        let chunk = history.record_chunk(sequence, ordinal, offset, 23)?;
        assert_eq!(chunk.history_sequence, sequence);
        assert_eq!(chunk.ordinal, ordinal);
        assert_eq!(chunk.offset, offset);
        assert!(chunk.bytes.len() <= 23);
        assert_eq!(
            source_sequence.get_or_insert(chunk.source_event_sequence),
            &chunk.source_event_sequence
        );
        assert_eq!(
            expected_size.get_or_insert(chunk.record_bytes),
            &chunk.record_bytes
        );
        record.extend_from_slice(&chunk.bytes);
        match chunk.next_offset {
            Some(next) => {
                assert_eq!(next, offset + chunk.bytes.len() as u64);
                assert!(!chunk.bytes.is_empty());
                offset = next;
            }
            None => {
                assert_eq!(offset + chunk.bytes.len() as u64, chunk.record_bytes);
                break;
            }
        }
    }
    Ok(record)
}

#[test]
fn provisional_index_slots_are_bounded_and_commit_after_data() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source = source();
    let history = History::live(root.path(), &source)?;
    let cell = HistoryCell {
        c: 'Ж',
        ..HistoryCell::default()
    };
    let row = HistoryRow::from_vec(vec![cell; 40], 40);
    history.begin(1)?;
    for _ in 0..1300 {
        history.row(&row, 40, HistoryRetirement::Scroll);
    }
    let path = root
        .path()
        .join(format!("{}.history/rows.index", source.generation));
    {
        let inner = history.inner.lock().unwrap();
        assert!(inner.store.index_buffer.len() <= INDEX_BUFFER_BYTES);
        assert!(fs::metadata(&path)?.len() < INDEX_HEADER + 1300 * INDEX_SLOT);
    }
    assert!(history.checkpoint().is_err());
    assert!(history.record(1299).is_err());
    history.commit()?;
    assert_eq!(fs::metadata(&path)?.len(), INDEX_HEADER + 1300 * INDEX_SLOT);
    assert!(history.inner.lock().unwrap().store.index_buffer.is_empty());
    let checkpoint = history.checkpoint()?;
    validate_checkpoint(root.path(), &source, &checkpoint)?;
    let (metadata, restored) = decode_record(&history.record(1299)?)?;
    assert_eq!(metadata.kind, Kind::Row);
    assert_eq!(restored.unwrap().cells[0].c, 'Ж');
    Ok(())
}

#[test]
fn cold_records_and_hot_parser_match_full_replay_checkpoint_and_live() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source = source();
    let path = root.path().join(format!("{}.journal", source.generation));
    let mut journal = Journal::create(&path, source.clone())?;
    let history = History::live(root.path(), &source)?;
    let mut terminal = Terminal::new(Geometry::new(8, 40)?, false);
    terminal.set_history_observer(Some(history.clone()));
    apply(
        &mut journal,
        &mut terminal,
        &history,
        EventKind::Output,
        b"\x1b]4;1;rgb:11/22/33\x07\x1b[31m\x1b]8;id=stable;https://example.test\x07",
    )?;
    let body = (0..11200)
        .map(|line| format!("{line:05} Русский e\u{301} 界\r\n"))
        .collect::<String>();
    for bytes in body.as_bytes().chunks(16384) {
        apply(
            &mut journal,
            &mut terminal,
            &history,
            EventKind::Output,
            bytes,
        )?;
    }
    assert!(history.summary().retired_rows > 1000);
    let cut = apply(
        &mut journal,
        &mut terminal,
        &history,
        EventKind::Output,
        b"\xf0\x9f",
    )?;
    crate::checkpoint::publish_with_history(
        root.path(),
        &source,
        cut,
        &terminal,
        Some(history.checkpoint()?),
    )?;
    apply(
        &mut journal,
        &mut terminal,
        &history,
        EventKind::Output,
        b"\x91\xa9\x1b[?1049hALTERNATE\r\n",
    )?;
    apply(
        &mut journal,
        &mut terminal,
        &history,
        EventKind::Resize,
        &serde_json::to_vec(&Geometry::new(8, 20)?)?,
    )?;
    apply(
        &mut journal,
        &mut terminal,
        &history,
        EventKind::Output,
        b"\x1b]4;1;rgb:aa/bb/cc\x07\x1b[?1049l\x1b[?2026hSYNC\r\n",
    )?;
    apply(
        &mut journal,
        &mut terminal,
        &history,
        EventKind::SyncTimeout,
        b"",
    )?;
    apply(
        &mut journal,
        &mut terminal,
        &history,
        EventKind::Output,
        b"\x1b[3J\x1bcFINAL",
    )?;
    apply(&mut journal, &mut terminal, &history, EventKind::Exit, b"")?;
    let (metadata, row) = decode_record(&history.record(0)?)?;
    assert_eq!(metadata.palette, [(1, [0x11, 0x22, 0x33])]);
    let row = row.unwrap();
    assert_eq!(row.columns, 40);
    assert_eq!(row.hyperlinks[0].id, "stable");
    assert!(row.cells.iter().any(|cell| cell.c == '界'));
    assert!(
        row.cells
            .iter()
            .any(|cell| cell.zerowidth.contains(&'\u{301}'))
    );
    assert_eq!(
        reassemble_chunks(&history, history.state().applied_sequence, 0)?,
        history.record(0)?,
        "chunk reads preserve canonical metadata, Unicode and hyperlink bytes"
    );
    let (_, replay, replay_history) = crate::restore_model(&path, false)?;
    assert_eq!(replay.checkpoint()?, terminal.checkpoint()?);
    let replay_history = replay_history.unwrap();
    assert_eq!(history.state(), replay_history.state());
    let base_end = replay_history
        .inner
        .lock()
        .expect("history lock")
        .base
        .as_ref()
        .map(StoreRoot::end)
        .transpose()?
        .unwrap_or(0);
    let replay_sequence = replay_history.state().applied_sequence;
    for ordinal in [0, base_end, replay_history.summary().records - 1] {
        assert_eq!(
            reassemble_chunks(&replay_history, replay_sequence, ordinal)?,
            replay_history.record(ordinal)?,
            "checkpoint-prefix and private-tail chunk record {ordinal}"
        );
    }
    let mut alternate_palette_observed = false;
    for ordinal in 0..history.summary().records {
        let encoded = history.record(ordinal)?;
        let (metadata, _) = decode_record(&encoded)?;
        alternate_palette_observed |= metadata.palette.contains(&(1, [0xaa, 0xbb, 0xcc]));
        assert_eq!(encoded, replay_history.record(ordinal)?, "record {ordinal}");
    }
    assert!(
        alternate_palette_observed,
        "palette changes in alternate screen must follow normal history"
    );
    let proof = crate::verify_recovery(&path)?;
    assert!(proof.complete_state_equal && proof.frame_equal);
    let live_proof = crate::verify_recovery_against_live(&path, &terminal, &history)?;
    assert_eq!(live_proof.live_state_equal, Some(true));
    assert_eq!(live_proof.live_frame_equal, Some(true));
    assert_eq!(live_proof.live_cold_history_equal, Some(true));
    let wrong_root = tempfile::tempdir()?;
    let wrong_history = History::live(wrong_root.path(), &source)?;
    assert!(crate::verify_recovery_against_live(&path, &terminal, &wrong_history).is_err());
    let header_bytes = pigade_storage::JournalReader::open(&path)?.bytes_read();
    assert_eq!(proof.full_journal_bytes_read, fs::metadata(&path)?.len());
    assert_eq!(
        proof.checkpoint_tail_journal_bytes_read,
        header_bytes + fs::metadata(&path)?.len() - cut.end_offset
    );
    assert!(proof.checkpoint_tail_journal_bytes_read < proof.full_journal_bytes_read / 100);
    let sequence = history.state().applied_sequence;
    let original = history.record(0)?;
    let directory = root.path().join(format!("{}.history", source.generation));
    let index_before = fs::read(directory.join("rows.index"))?;
    let segment_before = fs::read(segment_path(&directory, 0))?;
    assert!(history.record_chunk(sequence + 1, 0, 0, 1).is_err());
    assert!(history.record_chunk(sequence, 0, 0, 0).is_err());
    assert!(
        history
            .record_chunk(sequence, 0, 0, MAX_RECORD_CHUNK_BYTES + 1)
            .is_err()
    );
    assert!(history.record_chunk(sequence, 0, u64::MAX, 1).is_err());
    assert!(history.record_chunk(sequence, u64::MAX, 0, 1).is_err());
    let end = history.record_chunk(sequence, 0, original.len() as u64, 1)?;
    assert!(end.bytes.is_empty());
    assert_eq!(end.next_offset, None);
    assert_eq!(end.record_bytes, original.len() as u64);
    history.begin(sequence + 1)?;
    assert!(history.record_chunk(sequence, 0, 0, 1).is_err());
    assert_eq!(fs::read(directory.join("rows.index"))?, index_before);
    assert_eq!(fs::read(segment_path(&directory, 0))?, segment_before);
    assert!(history.summary().epoch >= 2);
    Ok(())
}

#[test]
fn segment_rotation_and_index_addressing_do_not_scan_unrelated_rows() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source = source();
    let history = History::live(root.path(), &source)?;
    let mut cell = HistoryCell::default();
    for _ in 0..100000 {
        cell.push_zerowidth('\u{301}');
    }
    let row = HistoryRow::from_vec(vec![cell, HistoryCell::default()], 1);
    history.begin(1)?;
    for _ in 0..24 {
        history.row(&row, 2, HistoryRetirement::Scroll);
    }
    history.commit()?;
    let saved = history.checkpoint()?;
    assert_eq!(saved.state.retired_rows, 24);
    let directory = saved.root.path(root.path())?;
    assert!(segment_path(&directory, 1).is_file());
    let expected = history.record(23)?;
    let mut offset = 0;
    let mut assembled = Vec::new();
    loop {
        let chunk = history.record_chunk(1, 23, offset, MAX_RECORD_CHUNK_BYTES)?;
        assert!(chunk.bytes.len() <= MAX_RECORD_CHUNK_BYTES);
        assembled.extend_from_slice(&chunk.bytes);
        match chunk.next_offset {
            Some(next) => {
                assert_eq!(next, offset + chunk.bytes.len() as u64);
                offset = next;
            }
            None => break,
        }
    }
    assert_eq!(assembled, expected);
    // A bad earlier slot cannot affect a direct lookup of a later row. Reading
    // that bad slot itself must fail rather than hiding unavailable history.
    let mut index = OpenOptions::new()
        .write(true)
        .open(directory.join("rows.index"))?;
    index.seek(SeekFrom::Start(INDEX_HEADER))?;
    index.write_all(&[0xff])?;
    assert!(history.record(0).is_err());
    let (_, decoded) = decode_record(&history.record(23)?)?;
    assert_eq!(decoded.unwrap().cells[0].zerowidth.len(), 100000);
    validate_checkpoint(root.path(), &source, &saved)?;
    let mut malicious = saved;
    malicious.root.directory = "../elsewhere".into();
    assert!(validate_checkpoint(root.path(), &source, &malicious).is_err());
    Ok(())
}

#[test]
fn raw_only_and_data_without_index_commit_rebuild_without_rewriting_originals() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source = source();
    let path = root.path().join(format!("{}.journal", source.generation));
    let mut journal = Journal::create(&path, source.clone())?;
    let history = History::live(root.path(), &source)?;
    let mut terminal = Terminal::new(Geometry::new(8, 40)?, false);
    terminal.set_history_observer(Some(history.clone()));
    let first = apply(
        &mut journal,
        &mut terminal,
        &history,
        EventKind::Output,
        b"BEFORE",
    )?;
    crate::checkpoint::publish_with_history(
        root.path(),
        &source,
        first,
        &terminal,
        Some(history.checkpoint()?),
    )?;
    let payload = b"\r\nAFTER\x1b[3J\x1bc";
    let receipt = journal.append(EventKind::Output, payload)?;
    history.begin(receipt.sequence)?;
    terminal.output(payload);
    history.inner.lock().unwrap().store.fail_before_index_sync = true;
    assert!(history.commit().is_err());
    assert!(history.checkpoint().is_err());
    assert!(history.begin(receipt.sequence + 1).is_err());
    let source_bytes = fs::read(&path)?;
    let archive_path = segment_path(
        &root.path().join(format!("{}.history", source.generation)),
        0,
    );
    let orphan_bytes = fs::read(&archive_path)?;
    let (_, recovered, recovered_history) = crate::restore_model(&path, true)?;
    assert_eq!(recovered.checkpoint()?, terminal.checkpoint()?);
    assert!(recovered_history.unwrap().summary().records > 0);
    assert_eq!(fs::read(&path)?, source_bytes);
    assert_eq!(fs::read(&archive_path)?, orphan_bytes);
    crate::verify_recovery(&path)?;
    Ok(())
}

#[test]
fn legacy_hot_checkpoint_with_valid_raw_rebuilds_cold_history() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source = source();
    let path = root.path().join(format!("{}.journal", source.generation));
    let mut journal = Journal::create(&path, source.clone())?;
    let mut terminal = Terminal::new(Geometry::new(8, 40)?, false);
    let bytes = b"old content\x1bcAFTER RESET";
    let receipt = journal.append(EventKind::Output, bytes)?;
    terminal.output(bytes);
    crate::checkpoint::publish(root.path(), &source, receipt, &terminal)?;
    let recovered = crate::recover_journal(&path)?;
    assert_eq!(recovered.recovery_path, "full-journal-legacy-checkpoint");
    assert!(recovered.storage_error.is_none());
    assert!(recovered.history.as_ref().unwrap().retired_rows >= 8);
    assert_eq!(recovered.frame, terminal.frame());
    assert_eq!(recovered.history, crate::replay_journal(&path)?.history);
    Ok(())
}
