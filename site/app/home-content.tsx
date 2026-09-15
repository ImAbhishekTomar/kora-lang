'use client'

import Link from 'next/link'
import { useRef, useState, type ReactNode } from 'react'

function ArrowRight({ dark = false }: { dark?: boolean }) {
  return <svg className="figma-small-icon" viewBox="0 0 14 14" aria-hidden="true"><path d="M3 7h8M8 4l3 3-3 3" stroke={dark ? '#121212' : '#c2d708'} strokeLinecap="round" strokeWidth="2" /></svg>
}

function SunIcon() {
  return <svg className="figma-sun" viewBox="0 0 18 18" aria-hidden="true"><circle cx="9" cy="9" r="3" stroke="#8e9aa8" strokeWidth="2" /><path d="M9 1v2M9 15v2M1 9h2M15 9h2M3.22 3.22l1.42 1.42M13.36 13.36l1.42 1.42M14.78 3.22l-1.42 1.42M4.64 13.36l-1.42 1.42" stroke="#8e9aa8" strokeLinecap="round" strokeWidth="2" /></svg>
}

function Navbar({ lightTheme, onToggleTheme, starCount }: { lightTheme: boolean; onToggleTheme: () => void; starCount: number }) {
  const [menuOpen, setMenuOpen] = useState(false)
  const links = [
    ['Docs', '/language'],
    ['Install', '/installation'],
    ['Examples', '/start-here'],
    ['Ecosystem', '/ecosystem'],
    ['Blog', '/releases'],
  ] as const
  return <header className="figma-navbar">
    <Link className="figma-logo" href="/"><img src="/logo-kora.png" alt="Kora" /></Link>
    <nav className="figma-nav-links" aria-label="Main navigation">{links.map(([label, href]) => <Link href={href} key={label}>{label}</Link>)}</nav>
    <div className="figma-nav-actions"><a className="figma-stars" href="https://github.com/ImAbhishekTomar/kora-lang" target="_blank" rel="noreferrer"><span>☆</span> {starCount.toLocaleString('en-US')} stars</a><button type="button" aria-label={lightTheme ? 'Switch to dark theme' : 'Switch to light theme'} aria-pressed={lightTheme} onClick={onToggleTheme}><SunIcon /></button><button className="figma-mobile-menu-button" type="button" aria-label={menuOpen ? 'Close navigation menu' : 'Open navigation menu'} aria-expanded={menuOpen} onClick={() => setMenuOpen(open => !open)}>☰</button></div>
    {menuOpen && <nav className="figma-mobile-menu" aria-label="Mobile navigation">{links.map(([label, href]) => <Link href={href} key={label} onClick={() => setMenuOpen(false)}>{label}</Link>)}</nav>}
  </header>
}

function HeroInfo() {
  return <div className="figma-hero-info">
    <div className="figma-badge"><i />PRE-ALPHA POLICY-SAFE WORKFLOWS</div>
    <h1>Keep sensitive AI<br />workflows inside<br />policy.</h1>
    <p>Kora is an experimental language for checked, replayable document workflows with explicit data boundaries.</p>
    <div className="figma-hero-actions"><Link className="figma-get-started" href="/start-here">Try a recorded workflow <ArrowRight dark /></Link><Link className="figma-explore" href="/comparison">Is Kora a fit?</Link></div>
  </div>
}

function TerminalHeader({ colored = false }: { colored?: boolean }) {
  return <div className="figma-terminal-header">{colored ? <><i className="red" /><i className="yellow" /><i className="green" /></> : <><i /><i /><i /></>}</div>
}

function Terminal({ command = false, output = '', running = false }: { command?: boolean; output?: string; running?: boolean }) {
  return <div className="figma-terminal"><TerminalHeader />{command ? <><div className="figma-terminal-command"><span>$</span> kora run --replay examples/03_salary_review.ko{running && <i className="figma-terminal-caret" aria-hidden="true" />}</div>{output && <pre className="figma-terminal-output" aria-live="polite">{output}</pre>}</> : <div className="figma-terminal-body"><div><span>$</span> brew install ImAbhishekTomar/tap/kora</div><div><span>$</span> kora check examples/03_salary_review.ko</div></div>}</div>
}

const editorLines: ReactNode[] = [
  <><b>type </b><strong>Employee</strong>:</>,
  <>    <em>name</em>: <strong className="blue">str</strong></>,
  <>    <em>role</em>: <strong className="blue">str</strong></>,
  <>    <b>classified </b><em>salary</em>: <strong className="blue">int</strong></>,
  <>&nbsp;</>,
  <><b>type </b><strong>Assessment</strong>:</>,
  <>    <em>band</em>: <strong className="blue">str</strong></>,
  <>    <em>rationale</em>: <strong className="blue">str</strong></>,
  <>&nbsp;</>,
  <><b>agent </b><span className="green">review</span>(<em>emp</em>: <strong>Employee</strong>) -&gt; <strong className="blue">str</strong>:</>,
  <>    <b>budget</b>: max_tokens = <span className="blue">4000</span></>,
  <>    <b>declassify </b><em>emp</em>.salary <b>as </b><em>pay</em> <b>for </b>local_model:</>,
  <>        <em>result</em>: <strong>Assessment</strong> = <span className="green">analyze</span>(</>,
  <>            {'{'}<span className="orange">"role"</span>: <em>emp</em>.role, <span className="orange">"pay"</span>: <em>pay</em>, <span className="orange">"market"</span>: <span className="green">market_rate</span>(<em>emp</em>.role){'}'},</>,
  <>            <span className="orange">"assess whether pay is below, at, or above market; band must be one of below/at/above"</span></>,
  <>        )</>,
  <>&nbsp;</>,
  <>    <b>match </b><em>result</em>:</>,
  <>        <b>case </b><strong>Ok</strong>(<em>a</em>):</>,
  <>            <b>return </b><span className="orange">f"{'{'}emp.name{'}'}: {'{'}a.band{'}'} - {'{'}a.rationale{'}'}"</span></>,
  <>        <b>case </b><strong>Uncertain</strong>(<em>why</em>):</>,
  <>            <b>return </b><span className="orange">f"human review: {'{'}why{'}'}"</span></>,
  <>        <b>case </b><strong>Exhausted</strong>(<em>meter</em>):</>,
  <>            <b>return </b><span className="orange">f"budget exhausted: {'{'}meter{'}'}"</span></>,
  <>        <b>case </b><strong>Failed</strong>(<em>why</em>):</>,
  <>            <b>return </b><span className="orange">f"provider failed: {'{'}why{'}'}"</span></>,
]

function CodeEditor() {
  return <div className="figma-code-editor"><div className="figma-editor-header"><div className="figma-window-controls"><i className="red" /><i className="yellow" /><i className="green" /></div><div><strong>K</strong> 03_salary_review.ko (excerpt)</div><span /></div><pre>{editorLines.map((line, index) => <code key={index}><small>{index + 1}</small><span>{line}</span></code>)}</pre></div>
}

const recordedRunOutput = `Ada: below - The pay of 165 is less than the market rate of 210.
Grace: above - The pay of 180 is higher than the market rate of 175.`

function InteractivePanel() {
  return <div className="figma-interactive">
    <Terminal /><CodeEditor /><Terminal command output={recordedRunOutput} />
    <div className="figma-eval-row"><button type="button" onClick={() => { window.location.href = '/start-here' }}>Run it locally</button><small>Output replayed from the committed cassette</small></div>
    <section className="figma-legacy-content" aria-label="Why Kora">
      <article><strong>01</strong><h2>Checked model calls</h2><p>Catch local type, call, field, and direct classified-flow mistakes before effects start.</p></article>
      <article><strong>02</strong><h2>Replayable runs</h2><p>Deterministic execution you can inspect, share, and replay.</p></article>
      <article><strong>03</strong><h2>Safe data flow</h2><p>Explicit data boundaries and policies to protect what matters.</p></article>
    </section>
    <section className="figma-detail-sections" aria-label="Kora capabilities">
      <article>
        <small>LANGUAGE / 01</small>
        <h2>Build agent workflows that read like programs.</h2>
        <p>Use ordinary control flow with typed model calls, explicit budgets, and outcomes you can handle instead of hidden failures.</p>
        <div className="figma-detail-pills"><span>typed</span><span>durable</span><span>composable</span></div>
      </article>
      <article>
        <small>RUNTIME / 02</small>
        <h2>Trace every step. Opt into durable execution.</h2>
        <div className="figma-trace-list"><div><i />plan <em>812ms</em></div><div><i />tool.web <em>1.23s</em></div><div><i className="warning" />provider.retry <em>replayed</em></div><div><i />return <em>complete</em></div></div>
      </article>
      <article>
        <small>SAFETY / 03</small>
        <h2>Classified data stays inside its boundary.</h2>
        <p>The checker catches direct unsafe model flow, and the runtime enforces the configured sink policy again when the call executes.</p>
        <div className="figma-policy-card"><span>classified</span><b>→</b><span>declassify</span><b>→</b><span>approved sink</span></div>
      </article>
    </section>
  </div>
}

export default function NewHomePage({ starCount }: { starCount: number }) {
  const [rightWidth, setRightWidth] = useState(62)
  const [lightTheme, setLightTheme] = useState(false)
  const dragging = useRef(false)
  return <main className={`figma-landing${lightTheme ? ' figma-theme-light' : ''}`} onMouseMove={event => { if (dragging.current) setRightWidth(Math.max(30, Math.min(65, 100 - event.clientX / window.innerWidth * 100))) }} onMouseUp={() => { dragging.current = false }}>
    <Navbar lightTheme={lightTheme} onToggleTheme={() => setLightTheme(theme => !theme)} starCount={starCount} />
    <div className="figma-main" style={{ gridTemplateColumns: `${100 - rightWidth}% 4px ${rightWidth}%` }}>
      <section className="figma-left-panel"><HeroInfo /></section>
      <button className="figma-resize" type="button" aria-label="Resize panels" onMouseDown={() => { dragging.current = true }} />
      <section className="figma-right-panel"><InteractivePanel /></section>
    </div>
  </main>
}
