import { describe, expect, mock, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type { On } from 'claude-code'

const ran = (stdout: string) => ({ exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false })

const state = (active: unknown) => JSON.stringify({ schema: 'tj-state/1', open_tasks: 1, active })

const TASK = { task_id: 'tj-abc', title: 'Fix refresh', counts: { decision: 2 }, modules: ['auth'] }

// The engine beneath the mod: `task-journal state` answers `stdout`, and
// another plugin's band sits under the mod's line.
function session(on: On, stdout: string) {
  on('session.start', (_$, e) => ({ cwd: e.cwd }))
  on('session.id', () => ({ value: 'session-1' }))
  on('process.run', () => ({ value: ran(stdout) }))
  on('ui.render', { component: 'AbovePrompt' }, ($, e) => $.ui.resolve(e).Text({ children: 'other band' }))
  mock.env(on, {})
}

// The band above the prompt as the terminal draws it, read as plain text.
async function band($: Engine): Promise<string> {
  const props = { hasSurvey: false, isWorking: false } as never
  const ui = await $.ui.mount({ plugin: 'task-journal', surface: 'terminal', component: 'AbovePrompt', props })
  const textOf = (node: unknown): string => {
    if (typeof node === 'string') return node
    const children = (node as { children?: unknown[] } | null)?.children ?? []

    return children.map(textOf).join('')
  }

  return textOf(await ui.drawn())
}

const START = { cwd: '/work', surface: 'terminal', isInteractive: true } as const

describe('band above the prompt', () => {
  test('names the session task above the other band', async ($, on) => {
    session(on, state(TASK))
    await $.session.start(START)

    expect(await band($)).toBe('📓 tj-abc [auth] · 2 decisions · 0 rejected · 0 evidence' + 'other band')
  })

  test('without a task the mod draws nothing of its own', async ($, on) => {
    session(on, state(null))
    await $.session.start(START)

    expect(await band($)).toBe('other band')
  })
})
