'use client'

import Link from 'next/link'
import { useEffect, useRef, useState, type ReactNode } from 'react'

function ArrowRight({ dark = false }: { dark?: boolean }) {
  return <svg className="figma-small-icon" viewBox="0 0 14 14" aria-hidden="true"><path d="M3 7h8M8 4l3 3-3 3" stroke={dark ? '#121212' : '#c2d708'} strokeLinecap="round" strokeWidth="2" /></svg>
}

function SunIcon() {
  return <svg className="figma-sun" viewBox="0 0 18 18" aria-hidden="true"><circle cx="9" cy="9" r="3" stroke="#8e9aa8" strokeWidth="2" /><path d="M9 1v2M9 15v2M1 9h2M15 9h2M3.22 3.22l1.42 1.42M13.36 13.36l1.42 1.42M14.78 3.22l-1.42 1.42M4.64 13.36l-1.42 1.42" stroke="#8e9aa8" strokeLinecap="round" strokeWidth="2" /></svg>
}

function Navbar({ lightTheme, onToggleTheme }: { lightTheme: boolean; onToggleTheme: () => void }) {
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
    <div className="figma-nav-actions"><a className="figma-stars" href="https://github.com/ImAbhishekTomar/kora-lang" target="_blank" rel="noreferrer"><span>☆</span> 0 stars</a><button type="button" aria-label={lightTheme ? 'Switch to dark theme' : 'Switch to light theme'} aria-pressed={lightTheme} onClick={onToggleTheme}><SunIcon /></button><button className="figma-mobile-menu-button" type="button" aria-label={menuOpen ? 'Close navigation menu' : 'Open navigation menu'} aria-expanded={menuOpen} onClick={() => setMenuOpen(open => !open)}>☰</button></div>
    {menuOpen && <nav className="figma-mobile-menu" aria-label="Mobile navigation">{links.map(([label, href]) => <Link href={href} key={label} onClick={() => setMenuOpen(false)}>{label}</Link>)}</nav>}
  </header>
}

function HeroInfo() {
  return <div className="figma-hero-info">
    <div className="figma-badge"><i />KORA IS A LANGUAGE, NOT A WRAPPER</div>
    <h1>Build AI agents<br />that finish what<br />they start.</h1>
    <p>Kora is a language for defining reliable AI workflows. Strongly typed, repayable, and safe by design — from prototype to production.</p>
    <div className="figma-hero-actions"><Link className="figma-get-started" href="/start-here">Get Started <ArrowRight dark /></Link><Link className="figma-explore" href="/language">Explore Guides</Link></div>
  </div>
}

function TerminalHeader({ colored = false }: { colored?: boolean }) {
  return <div className="figma-terminal-header">{colored ? <><i className="red" /><i className="yellow" /><i className="green" /></> : <><i /><i /><i /></>}</div>
}

function Terminal({ command = false, output = '', running = false }: { command?: boolean; output?: string; running?: boolean }) {
  return <div className="figma-terminal"><TerminalHeader />{command ? <><div className="figma-terminal-command"><span>$</span> kora run agent_classify_receipt.ko{running && <i className="figma-terminal-caret" aria-hidden="true" />}</div>{output && <pre className="figma-terminal-output" aria-live="polite">{output}</pre>}</> : <div className="figma-terminal-body"><div><span>$</span> brew tap ImAbhishekTomar/tap</div><div><span>$</span> brew install imabhishektomar/tap/kora</div></div>}</div>
}

const editorLines: ReactNode[] = [
  <><b>use</b> fs</>,
  <>&nbsp;</>,
  <><b>type </b><strong>Receipt</strong>:</>,
  <>    <em>merchant</em>: <strong className="blue">str</strong> <span className="blue">@description</span>(<span className="orange">"This is a salaer name"</span>)</>,
  <>    <b>classified </b><em>amount</em>: <strong className="blue">float</strong> <span className="comment"># Protected by the compiler: cannot be read directly</span></>,
  <>    <em>currency</em>: <strong className="blue">str</strong></>,
  <>    <em>review_reason</em>: <strong className="blue">str</strong></>,
  <>&nbsp;</>,
  <><b>def </b><span className="green">agent_classify_receipt</span>(<em>text</em>: <strong className="blue">str</strong>) -&gt; <strong className="blue">str</strong>:</>,
  <>    <em>receipt</em>: <strong>Receipt</strong> = <span className="green">analyze</span>(<em>text</em>, <span className="orange">"Extract receipt fields. Dates as YYYY-MM-DD."</span>)</>,
  <>&nbsp;</>,
  <>    <b>match </b><em>receipt</em>:</>,
  <>        <b>case </b><strong>Ok</strong>(<em>r</em>):</>,
  <>            <b>if </b><em>r</em>.needs_review:</>,
  <>                <b>return </b><span className="orange">f"REVIEW {'{'}r.merchant{'}'}: {'{'}r.amount{'}'} {'{'}r.currency{'}'}"</span></>,
  <>            <b>return </b><span className="orange">f"OK {'{'}r.merchant{'}'}: {'{'}r.amount{'}'} on {'{'}r.purchase_date{'}'}"</span></>,
  <>        <b>case </b><strong>Uncertain</strong>(<em>reason</em>):</>,
  <>            <b>return </b><span className="orange">f"SKIP could not classify receipt: {'{'}reason{'}'}"</span></>,
  <>        <b>case </b><strong>Exhausted</strong>(<em>meter</em>):</>,
  <>            <b>return </b><span className="orange">f"SKIP budget exhausted: {'{'}meter{'}'}"</span></>,
  <>        <b>case </b><strong>Failed</strong>(<em>why</em>):</>,
  <>            <b>return </b><span className="orange">f"RETRY provider did not answer: {'{'}why{'}'}"</span></>,
  <>&nbsp;</>,
  <><b>def </b><span className="green">main</span>():</>,
  <>    <b>match </b><em>fs</em>.<span className="green">read</span>(<span className="orange">"examples/receipts/sample.txt"</span>):</>,
  <>        <b>case </b><strong>Ok</strong>(<em>text</em>):</>,
  <>            <span className="green">print</span>(<span className="green">agent_classify_receipt</span>(<em>text</em>))</>,
  <>        <b>case </b><strong>Err</strong>(<em>reason</em>):</>,
  <>            <span className="green">print</span>(<span className="orange">f"Could not read receipt: {'{'}reason{'}'}"</span>)</>,
]

function CodeEditor() {
  return <div className="figma-code-editor"><div className="figma-editor-header"><div className="figma-window-controls"><i className="red" /><i className="yellow" /><i className="green" /></div><div><strong>K</strong> agent_classify_receipt.ko</div><span /></div><pre>{editorLines.map((line, index) => <code key={index}><small>{index + 1}</small><span>{line}</span></code>)}</pre></div>
}

const mockRunOutput = `✓ Loaded agent_classify_receipt.ko
→ Reading examples/receipts/sample.txt
→ Running analyze: Extract receipt fields
✓ merchant: Kora Coffee
✓ amount: classified (protected)
✓ currency: USD
✓ Run complete in 812ms`

function InteractivePanel() {
  const [runState, setRunState] = useState<'idle' | 'running' | 'complete'>('idle')
  const [output, setOutput] = useState('')

  useEffect(() => {
    if (runState !== 'running') return
    let cursor = 0
    setOutput('')
    const timer = window.setInterval(() => {
      cursor += 1
      setOutput(mockRunOutput.slice(0, cursor))
      if (cursor >= mockRunOutput.length) {
        window.clearInterval(timer)
        setRunState('complete')
      }
    }, 22)
    return () => window.clearInterval(timer)
  }, [runState])

  const runKora = () => {
    if (runState === 'running') return
    setOutput('')
    setRunState('running')
  }

  return <div className="figma-interactive">
    <Terminal /><CodeEditor /><Terminal command output={output} running={runState === 'running'} />
    <div className="figma-eval-row"><button type="button" disabled={runState === 'running'} onClick={runKora}><span className={runState === 'running' ? 'figma-run-spinner' : ''}>{runState === 'running' ? '◌' : '▷'}</span> {runState === 'running' ? 'Running...' : runState === 'complete' ? 'Run Again' : 'Run Kora'}</button><small>{runState === 'running' ? 'Streaming output...' : runState === 'complete' ? 'Run completed successfully' : 'Click to run this example'}</small></div>
    <section className="figma-legacy-content" aria-label="Why Kora">
      <article><strong>01</strong><h2>Typed model calls</h2><p>Define inputs and outputs. Catch issues at compile time, not at runtime.</p></article>
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
        <h2>Trace every step. Resume without losing the run.</h2>
        <div className="figma-trace-list"><div><i />plan <em>812ms</em></div><div><i />tool.web <em>1.23s</em></div><div><i className="warning" />provider.retry <em>replayed</em></div><div><i />return <em>complete</em></div></div>
      </article>
      <article>
        <small>SAFETY / 03</small>
        <h2>Classified data stays inside its boundary.</h2>
        <p>The compiler tracks sensitive values through the workflow and requires an explicit declassification before they reach a protected sink.</p>
        <div className="figma-policy-card"><span>classified</span><b>→</b><span>declassify</span><b>→</b><span>approved sink</span></div>
      </article>
    </section>
  </div>
}

export default function NewHomePage() {
  const [rightWidth, setRightWidth] = useState(62)
  const [lightTheme, setLightTheme] = useState(false)
  const dragging = useRef(false)
  return <main className={`figma-landing${lightTheme ? ' figma-theme-light' : ''}`} onMouseMove={event => { if (dragging.current) setRightWidth(Math.max(30, Math.min(65, 100 - event.clientX / window.innerWidth * 100))) }} onMouseUp={() => { dragging.current = false }}>
    <Navbar lightTheme={lightTheme} onToggleTheme={() => setLightTheme(theme => !theme)} />
    <div className="figma-main" style={{ gridTemplateColumns: `${100 - rightWidth}% 4px ${rightWidth}%` }}>
      <section className="figma-left-panel"><HeroInfo /></section>
      <button className="figma-resize" type="button" aria-label="Resize panels" onMouseDown={() => { dragging.current = true }} />
      <section className="figma-right-panel"><InteractivePanel /></section>
    </div>
  </main>
}
