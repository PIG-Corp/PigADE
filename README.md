# PigADE

**PigADE — Agentic Development Environment:** настольная среда для работы с несколькими CLI-агентами в одном проекте.

```text
Проект: общий Git-репозиторий и связанные части продукта
└── Воркспейс: задача, агенты, общий промпт, роли и память
    └── Вкладка: сохранённая раскладка
        └── Панели: Claude Code, Codex, OpenCode, shell и другие CLI
```

Текущая версия: **dev 0.0.1** (`0.0.1-dev`, [VERSION](VERSION)). Реализуется **этап 0 — технический прототип**. Готового MVP пока нет; критерии SPEC не ослаблены. В host реализованы каталог проектов/воркспейсов/вкладок и независимая модель tiling с сохранённым undo/redo. Их полный UI, подготовка Git worktree и контекст/адаптеры ещё не завершены. Прототип не является Linux beta.

## Перед работой

- [AGENTS.md](AGENTS.md) — обязательные правила для каждого агента, включая собственную запись в общий журнал.
- [SPEC.md](SPEC.md) — поведение, архитектура, этапы и критерии приёмки; прочитать полностью.
- [DESIGN.md](DESIGN.md) — UX, визуальный язык и доступность; прочитать полностью.
- [Карта проекта](docs/PROJECT_MAP.md), [текущий handoff](docs/AGENT_HANDOFF.md), [WORKLOG](WORKLOG.md), [CHANGELOG](CHANGELOG.md), [версии](docs/VERSIONING.md).

## Что уже можно проверить

Native Rust host запускает реальные процессы через portable-pty. Единственная модель alacritty/VTE находится в host; output/resize/timer events получают sequence и generation, сохраняются с CRC и fsync до публикации клеток. Есть bounded очереди, отдельный input writer, private Unix IPC, durable launch intent/helper/OS identity gate и блокировка неизвестного исхода запуска. После потери host восстановление историческое; процессы и задания сами не повторяются.

Экспериментальный полный checkpoint объединяет Term/Grid и parser/processor: оба экрана, горячую историю, курсоры, modes, tabstops, цвета, частичные UTF-8/CSI/OSC, synchronized buffer и логический timer. Host также архивирует вытесненные строки normal screen как производные записи PIGROW02 в поколенческие сегменты с фиксированным индексом; PIGCP002 привязывает только fsync-подтверждённый индексный корень. Журнал остаётся источником восстановления и не удаляется. Полное восстановление сравнивает горячую модель/parser и каждую холодную запись. Это реализованная основа, а не приёмка длинной истории: страничный пользовательский API, квоты, глобальный heap budget и long-history gate ещё не завершены.

[Приложение PigADE](crates/app/README.md) использует GPUI и получает клеточную реплику Rust host. Собственного ANSI-парсера в UI нет. Отдельные контрольные приложения удалены по решению пользователя; история раннего сравнения сохранена в Git. Это рабочее направление разработки, с открытыми gate корректности и платформенной поддержки.

## Сборка и проверки

Проверенная среда: Linux x86_64, Rust/Cargo 1.98.1, C compiler для bundled SQLite. Для UI нужны системные библиотеки GPUI из его официального Linux setup. Другие compiler/distro/platform версии не сертифицированы. Cargo.lock в корне и crates/app/Cargo.lock фиксируют зависимости host и UI. Лицензия продукта пока не выбрана; лицензии vendored компонентов сохранены, GPL terminal-код Zed не заимствован.

```sh
cargo build --release --workspace --bins --examples
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 scripts/check-version.py
```

Для UI: `crates/app/bootstrap.sh build --locked` и `crates/app/bootstrap.sh test --locked`. Bootstrap проверяет SHA-256 закреплённого GPUI archive. Он только собирает код, без открытия окон. `bash scripts/check.sh` выполняет проверки host/model workspace. Полные vendor/regression-команды и фактические результаты находятся в patch notes и отчётах.

## Запуск Rust host

Без systemd, в отдельном терминале:

```sh
mkdir -p "${XDG_STATE_HOME:-$HOME/.local/state}/pigade/dev-0.0.1"
target/release/pigade-session-host serve "${XDG_STATE_HOME:-$HOME/.local/state}/pigade/dev-0.0.1"
```

Управляемый user service, отдельный от UI:

```sh
bash scripts/start-dev-host.sh
systemctl --user status pigade-dev-host
```

Пример реального shell в host:

```sh
target/release/pigade-session-host launch \
  "${XDG_STATE_HOME:-$HOME/.local/state}/pigade/dev-0.0.1/host.sock" \
  "$PWD" /usr/bin/bash --noprofile --norc
```

`list SOCKET`, `request SOCKET JSON_COMMAND`, `replay JOURNAL` и `verify-recovery JOURNAL` — диагностический интерфейс. Input/resize/stop требуют точные session/generation UUID. `replay` не исполняет команды и terminal side effects. `verify-recovery` сравнивает полные модели полного replay и checkpoint+tail; нужен завершённый или неизменяемый журнал. Историческая панель не принимает ввод. Версия: `target/release/pigade-session-host --version`.

Остановка user service завершает его дочерние процессы; это отдельное явное действие: `systemctl --user stop pigade-dev-host`. Закрытие UI отличается от остановки host. После сбоя неизвестные launch intents остаются заблокированы; автоматического нового shell/resume нет.

## Измерения и ограничения

```sh
python3 scripts/hardware-manifest.py
python3 scripts/run-component-benchmark.py \
  --directory "${XDG_STATE_HOME:-$HOME/.local/state}/pigade/benchmarks"
python3 scripts/probe-cli-pty.py \
  --directory "${XDG_STATE_HOME:-$HOME/.local/state}/pigade/probes"
```

Benchmark требует durable filesystem, отклоняет tmpfs и измеряет компоненты без GPU/UI; его числа не равны input-to-present. CLI probe запускает только `--version`/`--help` через реальный PTY, не выполняет AI-задачи и не сертифицирует resume. Нагрузочная проверка: `python3 scripts/stress-pty.py --directory /absolute/durable/path`. Она не открывает UI и не является input-to-present benchmark. [Текущий отчёт о проверках](docs/reports/verification-0.0.1-dev.md) содержит результаты и точные пробелы.

Сырые результаты: [benchmarks/results](benchmarks/results), [CLI matrix](docs/compatibility/CLI.md), [ADR](docs/adr), [отчёты](docs/reports).

Приложение вручную запускается как `crates/app/target/debug/pigade-app /absolute/state/host.sock` после запуска host. Закрытие окна не завершает host. Автоматизация окон/ввода на активном desktop запрещена; platform tests требуют изолированного compositor/VM.

Открыты gate длинной истории (4×1 GiB, 100 000 resize), provider/layout части VM восстановления, 24h real-time soak, реальные диалоги трёх AI CLI/две версии каждого, Wayland/X11, IME/clipboard/fractional scale и accessibility, product-wide performance и сквозной MVP-сценарий. Новый index-batch запуск записал каждый 60 MiB producer body за 59.9845–59.9846 s и подтвердил полное равенство горячего состояния/parser/frame и всех 539 088 холодных записей в каждой из четырёх сессий; однако host exit занял 60.118–60.400 s, а schedule lateness составил 7.1–7.5 s. Это не steady-rate/latency acceptance. Пиковая sampled RSS host — 218 050 560 B без UI/GPU/provider и Python producers. Отдельный 8-session idle замер показал host-only 0.0% steady CPU и 319 963 136 B peak RSS. Новый smoke прогнал четыре 1 MiB histories по 100 resize с общим recovery root, SHA-256 и logical 24h metadata; это не 1 GiB/24h gate. Изолированный VM run подтвердил два boot и восстановление 2 467 холодных записей в каждой из четырёх сессий; это синтетический PTY тест, не provider resume или product layout acceptance. Последний сообщённый прогон: 120 workspace tests (fresh full rerun pending), vendor 152 tests + 45 reference cases; app 17 tests/all-target strict Clippy/build и изолированный software-rendered render/reopen с теми же PID/generation. GPU и platform acceptance не заявлены. Оставшиеся gates не пройдены. Подробности: [отчёт cold history](docs/reports/cold-history-integration.md) и [отчёт VM reboot](docs/reports/vm-reboot.md).
