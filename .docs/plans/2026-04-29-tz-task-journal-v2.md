# Техническое задание v2: Task Journal для AI-coding агентов

> **Источник**: пользовательское сообщение от 2026-04-29.
> **Статус**: канонический референс. Не правится без `bd update --supersede`.
> **Note**: оригинальный текст обрывается на разделе 2.1 ("High-level схема"). Архитектурные детали ниже раздела 2.1 будут написаны после brainstorm 9 открытых вопросов.

---

## Изменения относительно v1 (10 пунктов от пользователя)

1. Добавить task_pack как главный MCP tool с двумя modes
2. Расширить event types: добавить hypothesis, finding, evidence, constraint
3. Добавить confidence/evidence_strength/source во все relevant events
4. Classifier при confidence < 0.85 создаёт suggested events, не финальные
5. Убрать discussion compaction через основного Claude из v1
6. Сократить MCP tools с 8 до 5
7. Beads → v2
8. Phases: 2-3 недели реалистично
9. Переформулировать principle про "zero discipline"
10. Добавить в начало ТЗ жирно: **главный output — task_pack**

---

## 0. ГЛАВНЫЙ ПРИНЦИП (читать первым)

**Главный продуктовый output этого инструмента — `task_pack`.**

Все events, classifier, hooks, SQLite и MCP существуют ради одной цели: чтобы через дни, недели или месяцы можно было через MCP-вызов получить готовый компактный текст с полной логической цепочкой задачи (цель, рассуждения, активные решения, отвергнутые варианты, доказательства, коммиты, следующие шаги) — и инжектировать это в контекст AI-агента.

Если в процессе реализации возникает выбор между "сделать красивую event schema" и "task_pack возвращает полезный текст" — выбираем второе. Если возникает выбор между "идеальный classifier" и "task_pack работает даже когда classifier ошибся" — выбираем второе.

Tool без хорошего task_pack — это база данных. Tool с хорошим task_pack — это continuity layer.

---

## 1. Контекст и проблема

### 1.1 Проблема

Разработчик работает с AI-coding агентами над несколькими проектами параллельно. В одной сессии может вести 5-20 задач одновременно. Существующие memory-инструменты (Anthropic Session Memory, claude-mem, MemPalace, Beads) либо хранят сессии, либо хранят issues без обсуждений, либо делают flat semantic search. Никто не хранит **логическую цепочку конкретной задачи** — путь от вопроса к решению, включая отвергнутые варианты с причинами.

В результате через 2 недели или 2 месяца, когда возвращаешься к задаче, **причины принятых решений потеряны**. Остался результат в коде, но не "почему именно так".

### 1.2 Цель

Автоматически вести **task journal** — append-only event log где каждая задача проходит lifecycle (open → discuss → decide → close → reopen → supersede). Через MCP tool `task_pack` агент в любой момент получает компактный текст с восстановленной логической цепочкой задачи.

### 1.3 Принципы

- **Resume pack first** — главный output это готовый текст, не raw данные
- **Zero manual discipline для обычного flow** — пользователь не должен помнить команды в нормальной работе
- **Explicit correction tools are first-class** — исправить ошибку классификатора должно быть просто и заметно
- **No pollution of main session context** — никаких компрессий через основного Claude
- **Append-only event log** — события не редактируются, корректировки через redirect events
- **Derived state** — SQLite пересоздаётся из event log в любой момент
- **Cross-session, cross-project, cross-machine**
- **Reasoning trail captured** — гипотезы, находки, доказательства как first-class events
- **Confidence-aware** — каждое событие имеет confidence/evidence_strength

### 1.4 Что инструмент **не** делает (v1)

- Не заменяет Anthropic Session Memory
- Не заменяет Beads (v2 — опциональная интеграция)
- Не делает auto-capture всех tool calls агента
- Не делает discussion compaction через основного Claude
- Не делает cloud sync
- Не делает web UI

---

## 2. Архитектура

### 2.1 High-level схема

> **TODO**: дописать после brainstorm 9 открытых вопросов.
> См. `.docs/decisions/` для решений по каждому вопросу.

---

## Открытые вопросы для brainstorm (9 пунктов)

1. **Tech stack** — Node.js/TypeScript или Python? От этого зависит выбор MCP SDK.
2. **Какие именно 5 MCP tools** (сокращены с 8 — какие выбрасываем?)
3. **Точная shape `task_pack`** — какие поля, какой формат (Markdown? JSON? оба?), две modes (compact/full?).
4. **Event schema** — JSON-структура, обязательные/опциональные поля, версионирование.
5. **SQLite schema** — таблицы для derived state.
6. **Classifier** — на каком LLM/модели, где живёт (отдельный процесс? inline?), что делает при confidence < 0.85.
7. **Storage layout** — где живёт event log (`.task-journal/events.jsonl`?), где SQLite, cross-machine sync стратегия (или v1 только локально?).
8. **Hooks integration** — как именно классификатор слушает Claude Code события (PostToolUse? UserPromptSubmit?).
9. **Phases breakdown** — "2-3 недели реалистично" — а декомпозиция?

---

## История

- **2026-04-29**: ТЗ v2 зафиксировано в проекте. Epic в beads: `claude-memory-d36`.
