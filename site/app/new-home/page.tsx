'use client'

import Link from 'next/link'
import { useRef, useState, type ReactNode } from 'react'

function ArrowRight({ dark = false }: { dark?: boolean }) {
  return <svg className="figma-small-icon" viewBox="0 0 14 14" aria-hidden="true"><path d="M3 7h8M8 4l3 3-3 3" stroke={dark ? '#121212' : '#c2d708'} strokeLinecap="round" strokeWidth="2" /></svg>
}

function SunIcon() {
  return <svg className="figma-sun" viewBox="0 0 18 18" aria-hidden="true"><circle cx="9" cy="9" r="3" stroke="#8e9aa8" strokeWidth="2" /><path d="M9 1v2M9 15v2M1 9h2M15 9h2M3.22 3.22l1.42 1.42M13.36 13.36l1.42 1.42M14.78 3.22l-1.42 1.42M4.64 13.36l-1.42 1.42" stroke="#8e9aa8" strokeLinecap="round" strokeWidth="2" /></svg>
}

function Navbar() {
  const links = ['Docs', 'Guides', 'Tutorials', 'Integrations', 'Enterprise', 'Changelog', 'Blog']
  return <header className="figma-navbar">
    <Link className="figma-logo" href="/new-home"><img src="/logo-kora.png" alt="Kora" /></Link>
    <nav className="figma-nav-links" aria-label="Main navigation">{links.map(link => <Link href={link === 'Docs' ? '/language' : '#'} key={link}>{link}</Link>)}</nav>
    <div className="figma-nav-actions">
      <Link href="/start-here">Join Community</Link>
      <Link className="figma-discord" href="/start-here"><span className="figma-message-icon">⌁</span> Discord</Link>
      <a className="figma-stars" href="https://github.com/ImAbhishekTomar/kora-lang" target="_blank" rel="noreferrer"><span>☆</span> 18.1k stars</a>
      <button type="button" aria-label="Change theme"><SunIcon /></button>
    </div>
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

function Terminal({ command = false }: { command?: boolean }) {
  return <div className="figma-terminal"><TerminalHeader />{command ? <div className="figma-terminal-command"><span>$</span> kora run agent_classify_receipt.ko</div> : <div className="figma-terminal-body"><div><span>$</span> brew tap ImAbhishekTomar/tap</div><div><span>$</span> brew install imabhishektomar/tap/kora</div></div>}</div>
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

function InteractivePanel() {
  return <div className="figma-interactive">
    <Terminal /><CodeEditor /><Terminal command />
    <div className="figma-eval-row"><button type="button"><span>▷</span> Run Kora</button><small>Click to see evaluation metrics report</small></div>
    <section className="figma-legacy-content" aria-label="Why Kora">
      <article><strong>01</strong><h2>Typed model calls</h2><p>Define inputs and outputs. Catch issues at compile time, not at runtime.</p></article>
      <article><strong>02</strong><h2>Replayable runs</h2><p>Deterministic execution you can inspect, share, and replay.</p></article>
      <article><strong>03</strong><h2>Safe data flow</h2><p>Explicit data boundaries and policies to protect what matters.</p></article>
    </section>
  </div>
}

export default function NewHomePage() {
  const [rightWidth, setRightWidth] = useState(58)
  const dragging = useRef(false)
  return <main className="figma-landing" onMouseMove={event => { if (dragging.current) setRightWidth(Math.max(30, Math.min(65, 100 - event.clientX / window.innerWidth * 100))) }} onMouseUp={() => { dragging.current = false }}>
    <Navbar />
    <div className="figma-main" style={{ gridTemplateColumns: `${100 - rightWidth}% 4px ${rightWidth}%` }}>
      <section className="figma-left-panel"><HeroInfo /></section>
      <button className="figma-resize" type="button" aria-label="Resize panels" onMouseDown={() => { dragging.current = true }} />
      <section className="figma-right-panel"><InteractivePanel /></section>
    </div>
  </main>
}
