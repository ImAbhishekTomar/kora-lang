import Link from 'next/link'
import { Code2, History, ShieldCheck } from 'lucide-react'

const features = [
  { icon: Code2, title: 'Typed model calls', copy: 'Define inputs and outputs. Catch issues at compile time, not at runtime.' },
  { icon: History, title: 'Replayable runs', copy: 'Deterministic execution you can inspect, share, and replay.' },
  { icon: ShieldCheck, title: 'Safe data flow', copy: 'Explicit data boundaries and policies to protect what matters.' }
]

const code = `pipeline Research(topic: string) {
  input: topic

  plan = llm.plan(topic)
  hits = web.search(plan.query)
  draft = llm.generate(plan, hits)
  review = llm.review(draft)
  if review.approved {
    return draft
  } else {
    return llm.revise(draft, review)
  }
}`

const trace = [
  ['plan', 'llm.plan', '812ms'],
  ['web.search', 'tool.web', '1.23s'],
  ['generate', 'llm.generate', '2.48s'],
  ['review', 'llm.review', '983ms'],
  ['return', 'Success', '—']
]

export default function HomePage() {
  return (
    <main className="kora-landing">
      <header className="landing-nav">
        <Link className="landing-logo" href="/"><img src="/kora-icon-s.svg" alt="" /><span>Kora</span></Link>
        <nav aria-label="Main navigation"><Link href="/language">Language</Link><Link href="/comparison">Why Kora</Link><a href="#community">Community</a><a href="https://github.com/ImAbhishekTomar/kora-lang">◉ GitHub</a></nav>
      </header>

      <section className="landing-hero-section">
        <div className="landing-hero-copy">
          <h1><span>Build AI agents</span><span>that keep their</span><span>promises.</span></h1>
          <p>Kora is a language for defining reliable AI workflows. Strongly typed, replayable, and safe by design - from prototype to production.</p>
          <div className="landing-ctas"><Link className="landing-primary" href="/start-here">Start building <b>→</b></Link><Link className="landing-secondary" href="/installation">Read the docs <b>→</b></Link></div>
        </div>
        <div className="landing-mascot-area" aria-hidden="true"><img src="/kora-mascot-graph.svg" alt="" /></div>
      </section>

      <section className="landing-proof" id="community">
        <div className="landing-feature-list">{features.map(feature => { const Icon = feature.icon; return <article key={feature.title}><span><Icon aria-hidden="true" strokeWidth={1.7} /></span><h2>{feature.title}</h2><p>{feature.copy}</p></article> })}</div>
        <div className="landing-run-card"><pre><code>{code}</code></pre><div className="landing-trace"><header><strong>Execution trace</strong><span>Replayed</span></header>{trace.map(([name, source, timing]) => <div key={name}><i>✓</i><b>{name}</b><small>{source}</small><time>{timing}</time></div>)}</div></div>
      </section>

      <footer className="landing-footer">✦ <span>Kora is a language, <em>not</em> a wrapper.</span> ✦</footer>
    </main>
  )
}
