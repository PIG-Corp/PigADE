//! Recovery-only protocol proof. Production actors do not yet perform resource
//! admission or emit refusal events.
use super::*;
use pigade_storage::scan_valid_prefix;

fn header(profile: String) -> JournalHeader {
    JournalHeader {
        session_id: Uuid::new_v4(),
        generation: Uuid::new_v4(),
        profile,
        initial_rows: 8,
        initial_cols: 40,
    }
}

fn apply_output(
    journal: &mut Journal,
    terminal: &mut Terminal,
    history: &history::History,
    bytes: &[u8],
) -> Result<pigade_storage::Receipt> {
    let receipt = journal.append(EventKind::Output, bytes)?;
    history.begin(receipt.sequence)?;
    terminal.output(bytes);
    history.commit()?;
    Ok(receipt)
}

#[test]
fn durable_refusal_keeps_complete_prefix_and_every_cold_record_in_both_recovery_paths() -> Result<()>
{
    for profile in [
        CORE_PROFILE.to_owned(),
        journal_policy::resource_refusal_profile(),
    ] {
        let root = tempfile::tempdir()?;
        let header = header(profile);
        let path = root.path().join(format!("{}.journal", header.generation));
        let mut journal = Journal::create(&path, header.clone())?;
        let history = history::History::live(root.path(), &header)?;
        let mut terminal = Terminal::new(Geometry::new(8, 40)?, false);
        terminal.set_history_observer(Some(history.clone()));
        let mut rows = b"cold row\r\n".repeat(10_020);
        rows.extend_from_slice("界e\u{301} prefix\r\n".as_bytes());
        let receipt = apply_output(&mut journal, &mut terminal, &history, &rows)?;
        assert!(
            history.summary().records > 0,
            "cold comparison must be nonvacuous"
        );
        checkpoint::publish_with_history(
            root.path(),
            &header,
            receipt,
            &terminal,
            Some(history.checkpoint()?),
        )?;

        // Leave alternate screen, color and parser continuation state after
        // the checkpoint, so a frame-only/prefix-only oracle is insufficient.
        let prefix = apply_output(
            &mut journal,
            &mut terminal,
            &history,
            "\x1b[?1049hALT界e\u{301}\x1b[38;2;12;".as_bytes(),
        )?;
        let expected = terminal.checkpoint()?;
        let expected_frame = terminal.frame();
        let expected_history = history.state();
        let rejected = b"34;56mMUST_NOT_APPLY\x1b[?1049l\x1b]52;c;REPLAY_SIDE_EFFECT\x07";
        let refusal = journal.append(EventKind::OutputRejectedResource, rejected)?;
        assert!(
            journal
                .append(EventKind::Output, b"MUST_NOT_SKIP_REFUSAL")
                .is_err()
        );
        assert!(checkpoint::publish(root.path(), &header, refusal, &terminal).is_err());
        assert!(checkpoint::schedule(root.path(), &header, refusal, &terminal, None).is_err());
        assert_eq!(refusal.sequence, prefix.sequence + 1);
        drop(journal);
        let source = fs::read(&path)?;
        let framing = scan_valid_prefix(&path)?;
        assert!(framing.corruption.is_none());
        assert_eq!(framing.valid_events, prefix.sequence + 1);

        let (full_view, mut full, full_history) = restore_model(&path, false)?;
        let (fast_view, mut fast, fast_history) = restore_model(&path, true)?;
        assert_eq!(fast_view.recovery_path, "checkpoint-tail");
        for (view, model) in [(&full_view, &full), (&fast_view, &fast)] {
            assert_eq!(view.sequence, prefix.sequence);
            assert_eq!(view.frame, expected_frame);
            assert_eq!(model.checkpoint()?, expected);
            let error = view
                .storage_error
                .as_deref()
                .context("missing explicit refusal")?;
            assert!(error.contains(&format!("sequence {}", refusal.sequence)));
            assert!(error.contains("raw tail retained"));
            if header.profile == CORE_PROFILE {
                assert!(error.contains("not permitted by legacy journal profile"));
            } else {
                assert!(error.contains("terminal resource budget refused"));
            }
        }
        let full_history = full_history.context("full cold history missing")?;
        let fast_history = fast_history.context("fast cold history missing")?;
        assert_eq!(full_history.state(), expected_history);
        assert_eq!(fast_history.state(), expected_history);
        for ordinal in 0..expected_history.next_entry {
            let expected_record = history.record(ordinal)?;
            assert_eq!(full_history.record(ordinal)?, expected_record);
            assert_eq!(fast_history.record(ordinal)?, expected_record);
        }
        assert!(
            verify_recovery(&path).is_err(),
            "refused recovery cannot pass clean gate"
        );
        assert_eq!(fs::read(&path)?, source, "raw/refused suffix was modified");

        // Continuing the recovered hidden parser and both grids must still
        // agree. Detach archives because this synthetic continuation is not a
        // new durable event and must not touch the committed source history.
        for model in [&mut terminal, &mut full, &mut fast] {
            model.set_history_observer(None);
            model.output(b"34;56mCONTINUE\x1b[?1049l\r\nNORMAL_CONTINUE");
            assert!(model.take_responses().is_empty());
        }
        assert_eq!(full.checkpoint()?, terminal.checkpoint()?);
        assert_eq!(fast.checkpoint()?, terminal.checkpoint()?);
        assert_eq!(full.frame(), terminal.frame());
        assert_eq!(fast.frame(), terminal.frame());
    }
    Ok(())
}

#[test]
fn first_event_refusal_preserves_empty_model_without_inventing_a_session_process() -> Result<()> {
    let root = tempfile::tempdir()?;
    let header = header(journal_policy::resource_refusal_profile());
    let path = root.path().join(format!("{}.journal", header.generation));
    let mut journal = Journal::create(&path, header)?;
    journal.append(EventKind::OutputRejectedResource, b"\x1b[?1049hNEVER_APPLY")?;
    drop(journal);
    let source = fs::read(&path)?;
    let expected = Terminal::new(Geometry::new(8, 40)?, false).checkpoint()?;
    for fast in [false, true] {
        let (view, model, history) = restore_model(&path, fast)?;
        assert_eq!(view.sequence, 0);
        assert_eq!(view.pid, None);
        assert_eq!(view.state, "history");
        assert!(
            view.storage_error
                .as_deref()
                .is_some_and(|s| s.contains("sequence 1"))
        );
        assert_eq!(model.checkpoint()?, expected);
        assert_eq!(history.context("history missing")?.state().next_entry, 0);
    }
    assert_eq!(fs::read(path)?, source);
    Ok(())
}

#[test]
fn new_journal_policy_keeps_existing_terminal_checkpoint_and_clean_replay_semantics() -> Result<()>
{
    let root = tempfile::tempdir()?;
    let header = header(journal_policy::resource_refusal_profile());
    let path = root.path().join(format!("{}.journal", header.generation));
    let mut journal = Journal::create(&path, header.clone())?;
    let history = history::History::live(root.path(), &header)?;
    let mut terminal = Terminal::new(Geometry::new(8, 40)?, false);
    terminal.set_history_observer(Some(history.clone()));
    let receipt = apply_output(
        &mut journal,
        &mut terminal,
        &history,
        b"SAFE\x1b[?1049hALT\x1b[38;2;12;",
    )?;
    checkpoint::publish_with_history(
        root.path(),
        &header,
        receipt,
        &terminal,
        Some(history.checkpoint()?),
    )?;
    apply_output(
        &mut journal,
        &mut terminal,
        &history,
        b"34;56mTAIL\x1b[?1049l",
    )?;
    drop(journal);
    let verification = verify_recovery_against_live(&path, &terminal, &history)?;
    assert!(verification.complete_state_equal && verification.live_state_equal == Some(true));
    assert_eq!(verification.recovery_path, "checkpoint-tail");
    Ok(())
}
