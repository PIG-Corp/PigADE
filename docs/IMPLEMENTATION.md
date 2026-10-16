# План реализации и gate

Исходное состояние: README/SPEC/DESIGN v0.1, исходников нет. Продуктовая лицензия не выбрана; сторонний GPL-код Zed не копируется.

## Последовательность

0. **Технический прототип (в работе).** Настоящий PTY → ordered durable journal → alacritty model → versioned cells; независимый host, resize, kill/reopen; клеточная GPUI-реплика основного приложения. Отдельные сравнительные стенды исключены пользователем. Проверка полного состояния и checkpoint+tail, platform/accessibility spike, release measurements. Стек не утверждается по одному replay.
1. **Терминальный фундамент.** Durable launch helper/identity/generation locks, fault injection, storage degradation/backpressure. TERM/REC/STO перед сложным chrome.
2. **Структура продукта.** Projects/roots/workspaces/worktrees/tabs, transactional persistence.
3. **Tiling.** Pure model, stable UUID, operations and 50 undo/redo, mouse/key parity, geometry checks.
4. **Контекст и адаптеры.** Immutable revisions, CAS memory, explicit capabilities, exact conversation binding/resume with receipts.
5. **UI и эксплуатация.** Native render replica, DESIGN tokens, accessibility, packaging, measured budgets.
6. **Linux beta.** Real CLI scenario plus VM reboot and long-history/24h soak; no release blockers.

Каждый этап закрывается свежими доказательствами. Непройденные проверки и недоступные средства указываются явно; следующий этап не подменяет gate предыдущего. Rust baseline: fmt, clippy -D warnings, workspace tests, release build. Версии фиксируют корневой Cargo.lock и crates/app/Cargo.lock.

## Жёсткие границы прототипа

Полный replay разрешён только на этапе 0 и остаётся correctness oracle. Cell snapshot предназначен для IPC, не считается полным checkpoint. Terminal responses разрешены только при live parsing; replay не исполняет side effects. Input не журналируется. AI-задачи и native resume не запускаются для проверки флагов. Непроверенные версии провайдера не получают capability `supported`.
