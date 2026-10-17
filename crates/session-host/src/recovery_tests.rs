use super::*;

#[test]
fn history_snapshot_stops_at_exact_published_watermark_with_newer_checkpoint() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let header = JournalHeader {
        session_id: Uuid::new_v4(),
        generation: Uuid::new_v4(),
        profile: CORE_PROFILE.into(),
        initial_rows: 3,
        initial_cols: 16,
    };
    let path = directory
        .path()
        .join(format!("{}.journal", header.generation));
    let mut journal = Journal::create(&path, header.clone())?;
    let history = history::History::live(directory.path(), &header)?;
    let mut terminal = Terminal::new(Geometry::new(3, 16)?, false);
    terminal.set_history_observer(Some(history.clone()));
    let mut expected = Vec::new();
    for bytes in [
        b"old normal\r\n".as_slice(),
        b"\x1b[?1049hnew alt".as_slice(),
    ] {
        let receipt = journal.append(EventKind::Output, bytes)?;
        history.begin(receipt.sequence)?;
        terminal.output(bytes);
        history.commit()?;
        expected.push(terminal.checkpoint()?);
        checkpoint::publish_with_history(
            directory.path(),
            &header,
            receipt,
            &terminal,
            Some(history.checkpoint()?),
        )?;
    }
    let raw_before = fs::read(&path)?;
    for sequence in 0..=2 {
        let (restored, archive, _) =
            restore_history_view(&path, header.session_id, header.generation, sequence)?;
        if sequence > 0 {
            ensure!(restored.checkpoint()? == expected[sequence as usize - 1]);
        } else {
            ensure!(
                restored.checkpoint()?
                    == Terminal::new(Geometry::new(3, 16)?, false).checkpoint()?
            );
        }
        ensure!(archive.state().applied_sequence == sequence);
    }
    ensure!(restore_history_view(&path, header.session_id, header.generation, 3).is_err());
    ensure!(restore_history_view(&path, Uuid::new_v4(), header.generation, 1).is_err());
    ensure!(
        fs::read(&path)? == raw_before,
        "history viewer changed raw source"
    );
    Ok(())
}

#[test]
fn rejected_tail_preserves_complete_hidden_state_and_continuation() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let header = JournalHeader {
        session_id: Uuid::new_v4(),
        generation: Uuid::new_v4(),
        profile: CORE_PROFILE.into(),
        initial_rows: 8,
        initial_cols: 40,
    };
    let path = directory
        .path()
        .join(format!("{}.journal", header.generation));
    let mut journal = Journal::create(&path, header)?;
    let history = history::History::live(directory.path(), journal.header())?;
    let mut terminal = Terminal::new(Geometry::new(8, 40)?, false);
    terminal.set_history_observer(Some(history.clone()));
    // Hidden normal screen, alternate screen, cursor mode and a partial UTF-8
    // scalar must survive rejection too; a screenshot cannot prove this.
    let output = b"NORMAL\x1b[?1049hALT\x1b[38;2;12;34;56m\x1b[?2004h\xf0\x9f";
    let receipt = journal.append(EventKind::Output, output)?;
    history.begin(receipt.sequence)?;
    terminal.output(output);
    history.commit()?;
    let expected = terminal.checkpoint()?;
    let expected_history = history.state();
    checkpoint::publish_with_history(
        directory.path(),
        journal.header(),
        receipt,
        &terminal,
        Some(history.checkpoint()?),
    )?;
    journal.append(EventKind::Resize, br#"{"rows":0,"cols":80}"#)?;
    drop(journal);
    let raw = fs::read(&path)?;
    let mut continued = Terminal::from_checkpoint(&expected, false)?;
    continued.output(&[0x90, 0x96]);
    for fast in [false, true] {
        let (view, mut recovered, history) = restore_model(&path, fast)?;
        ensure!(view.sequence == 1 && view.storage_error.is_some());
        ensure!(recovered.checkpoint()? == expected, "hidden state changed");
        ensure!(history.context("missing history")?.state() == expected_history);
        recovered.output(&[0x90, 0x96]);
        ensure!(
            recovered.checkpoint()? == continued.checkpoint()?,
            "partial UTF-8 continuation changed"
        );
        ensure!(fs::read(&path)? == raw, "source tail changed");
    }
    Ok(())
}
