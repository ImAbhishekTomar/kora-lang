import Link from 'next/link'
import { Check, Code2, GitBranch, History, ShieldCheck, X } from 'lucide-react'

const features = [
  { icon: Code2, title: 'Typed model calls', copy: 'Define inputs and outputs. Catch issues at compile time, not at runtime.' },
  { icon: History, title: 'Replayable runs', copy: 'Deterministic execution you can inspect, share, and replay.' },
  { icon: ShieldCheck, title: 'Safe data flow', copy: 'Explicit data boundaries and policies to protect what matters.' }
]

const trace = [
  ['plan', 'llm.plan', '812ms', 'success'],
  ['web.search', 'tool.web', '1.23s', 'success'],
  ['generate', 'llm.generate', '2.48s', 'error'],
  ['review', 'llm.review', '983ms', 'success'],
  ['return', 'Success', '—', 'success']
] as const

function HighlightedCode() {
  return <>
    <span className="token-keyword">pipeline</span> <span className="token-name">Research</span>(<span className="token-name">topic</span>: <span className="token-type">string</span>) {'{'}
    {'\n  '}<span className="token-keyword">input</span>: <span className="token-name">topic</span>
    {'\n\n  '}<span className="token-name">plan</span> = <span className="token-call">llm.plan</span>(<span className="token-name">topic</span>)
    {'\n  '}<span className="token-name">hits</span> = <span className="token-call">web.search</span>(<span className="token-name">plan.query</span>)
    {'\n  '}<span className="token-name">draft</span> = <span className="token-call">llm.generate</span>(<span className="token-name">plan</span>, <span className="token-name">hits</span>)
    {'\n  '}<span className="token-name">review</span> = <span className="token-call">llm.review</span>(<span className="token-name">draft</span>)
    {'\n  '}<span className="token-keyword">if</span> <span className="token-name">review.approved</span> {'{'}
    {'\n    '}<span className="token-keyword">return</span> <span className="token-name">draft</span>
    {'\n  } '}<span className="token-keyword">else</span> {'{'}
    {'\n    '}<span className="token-keyword">return</span> <span className="token-call">llm.revise</span>(<span className="token-name">draft</span>, <span className="token-name">review</span>)
    {'\n  }\n'}{'}'}
  </>
}

export default function HomePage() {
  return (
    <main className="kora-landing">
      <header className="landing-nav">
        <Link className="landing-logo" href="/"><img src="/kora-icon-s.svg" alt="" /><span>Kora</span></Link>
        <nav aria-label="Main navigation"><Link href="/language">Language</Link><Link href="/comparison">Why Kora</Link><a href="#community">Community</a><a href="https://github.com/ImAbhishekTomar/kora-lang" target="_blank" rel="noreferrer"><GitBranch aria-hidden="true" size={19} /> GitHub</a></nav>
      </header>

      <section className="landing-hero-section">
        <div className="landing-hero-copy">
          <h1><span>Build AI agents</span><span>that keep their</span><span>promises.</span></h1>
          <p className="landing-summary">Kora is a language for defining reliable AI workflows. Strongly typed, replayable, and safe by design — from prototype to production.</p>
          <div className="landing-ctas"><Link className="landing-primary" href="/start-here">Start building <b>→</b></Link><Link className="landing-secondary" href="/installation">Read the docs <b>→</b></Link></div>
        </div>
        <div className="landing-mascot-area" aria-hidden="true"><img src="/kora-mascot-graph.svg" alt="" /></div>
      </section>

      <section className="landing-proof" id="community">
        <div className="landing-feature-list">{features.map(feature => { const Icon = feature.icon; return <article key={feature.title}><span><Icon aria-hidden="true" strokeWidth={1.7} /></span><h2>{feature.title}</h2><p>{feature.copy}</p></article> })}</div>
        <div className="landing-run-card"><pre aria-label="Kora pipeline code"><code><HighlightedCode /></code></pre><div className="landing-trace"><header><strong>Execution trace</strong><span>Replayed</span></header>{trace.map(([name, source, timing, status]) => <div className={`trace-step trace-${status}`} key={name}><i aria-label={status === 'error' ? 'Failed' : 'Completed'}>{status === 'error' ? <X aria-hidden="true" /> : <Check aria-hidden="true" />}</i><b>{name}</b><small>{source}</small><time>{timing}</time></div>)}</div></div>
      </section>

      <footer className="landing-footer">✦ <span>Kora is a language, <em>not</em> a wrapper.</span> ✦</footer>
    </main>
  )
}
