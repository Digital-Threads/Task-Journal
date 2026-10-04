import { describe, expect, test } from 'claude-code/testing'

import {
  journalTool,
  nudgeText,
  parseDistill,
  parseState,
  sectionText,
  statusText,
  type JournalState,
} from './journal'

const ACTIVE = JSON.stringify({
  schema: 'tj-state/1',
  open_tasks: 2,
  active: {
    task_id: 'tj-abc',
    title: 'Fix refresh',
    goal: 'Stop dropping the token',
    counts: { decision: 3, rejection: 1, evidence: 0 },
    recent: [{ type: 'decision', text: 'Use <= on expiry' }],
  },
})

describe('parseState', () => {
  test('reads the active task', () => {
    const s = parseState(ACTIVE)
    expect(s?.active?.task_id).toBe('tj-abc')
    expect(s?.active?.recent.length).toBe(1)
    expect(s?.open_tasks).toBe(2)
  })

  test('no active task is a state, not a failure', () => {
    const s = parseState(JSON.stringify({ schema: 'tj-state/1', open_tasks: 0, active: null }))
    expect(s).toEqual({ active: null, open_tasks: 0 })
  })

  test('rejects garbage and other schemas', () => {
    expect(parseState('error: unrecognized subcommand')).toBe(null)
    expect(parseState(JSON.stringify({ schema: 'tj-state/9', active: null }))).toBe(null)
  })

  test('an empty goal reads as not set', () => {
    const s = parseState(JSON.stringify({ schema: 'tj-state/1', open_tasks: 1, active: { task_id: 'tj-x', goal: ' ' } }))
    expect(s?.active?.goal).toBe(null)
    expect(s?.active?.title).toBe('tj-x')
  })
})

describe('texts', () => {
  const s = parseState(ACTIVE) as JournalState
  const none: JournalState = { active: null, open_tasks: 3 }

  test('status line names the task and its counts', () => {
    expect(statusText(s)).toBe('📓 tj-abc · 3 decisions · 1 rejected · 0 evidence')
    expect(statusText(none)).toBe('📓 no task in this session · 3 open')
    expect(statusText({ active: null, open_tasks: 0 })).toBe('📓 no task')
  })

  test('the prompt section holds nothing that changes per entry', () => {
    const text = sectionText(s)
    expect(text).toContain('tj-abc')
    expect(text).toContain('Stop dropping the token')
    expect(text).not.toContain('3 decisions')
    expect(text).not.toContain('Use <= on expiry')
  })

  test('nudge names the task, or asks to open one', () => {
    expect(nudgeText(s, 6)).toContain('6 turns since the last journal entry on tj-abc')
    expect(nudgeText(none, 6)).toContain('task_create')
  })
})

describe('journalTool', () => {
  test('matches the plugin server and a hand-wired one', () => {
    expect(journalTool('mcp__plugin_task-journal_task-journal__event_add')).toBe('event_add')
    expect(journalTool('mcp__task-journal__task_close')).toBe('task_close')
    expect(journalTool('mcp__other__event_add')).toBe(undefined)
    expect(journalTool('Bash')).toBe(undefined)
  })
})

describe('parseDistill', () => {
  const known = [{ type: 'decision', text: 'Use <= on expiry' }]

  test('keeps typed, new events from a fenced reply', () => {
    const reply = 'Here:\n```json\n[{"type":"rejection","text":"Ruled out a cache: stale tokens"},{"type":"finding","text":"refresh.rs:42 uses <"}]\n```'
    expect(parseDistill(reply, known)).toEqual([
      { type: 'rejection', text: 'Ruled out a cache: stale tokens' },
      { type: 'finding', text: 'refresh.rs:42 uses <' },
    ])
  })

  test('drops known, duplicate, untyped and empty entries', () => {
    const reply = JSON.stringify([
      { type: 'decision', text: '  use <=  ON expiry ' },
      { type: 'open', text: 'not a distill type' },
      { type: 'finding', text: '' },
      { type: 'finding', text: 'A' },
      { type: 'finding', text: 'a' },
      'junk',
    ])
    expect(parseDistill(reply, known)).toEqual([{ type: 'finding', text: 'A' }])
  })

  test('caps the count and the length', () => {
    const many = Array.from({ length: 20 }, (_, i) => ({ type: 'finding', text: `f${i} ${'x'.repeat(600)}` }))
    const out = parseDistill(JSON.stringify(many), [])
    expect(out.length).toBe(8)
    expect(out[0]?.text.length).toBe(500)
  })

  test('no array, no events', () => {
    expect(parseDistill('Nothing to add.', known)).toEqual([])
    expect(parseDistill('[not json', known)).toEqual([])
  })
})
