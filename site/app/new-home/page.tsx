import Link from 'next/link'
import { ArrowUpRight, Check, CirclePlay, Code2, GitBranch, Menu, ShieldCheck, Sparkles, Terminal, Users } from 'lucide-react'

const features = [
  { icon: ShieldCheck, title: 'Typed by default', copy: 'Catch bad assumptions before they reach production.' },
  { icon: Sparkles, title: 'Built for agents', copy: 'Give every workflow a clear, durable shape.' },
  { icon: Users, title: 'Human in the loop', copy: 'Keep people in control when decisions matter.' },
  { icon: Code2, title: 'Easy to read', copy: 'Write agent systems that feel like ordinary code.' },
]

function CodePanel() {
  return (
    <div className="new-landing-code-stack" aria-label="Kora code example">
      <div className="new-landing-terminal-bar"><span /><span /><span /><code>kora run examples/research.ko</code></div>
      <div className="new-landing-code-panel">
        <div className="code-line"><i>1</i><span><b className="code-purple">agent</b> <b className="code-blue">Researcher</b>(topic: <b className="code-yellow">string</b>) {'{'}</span></div>
        <div className="code-line"><i>2</i><span>&nbsp;&nbsp;<b className="code-purple">with</b> budget(max_tokens: <b className="code-green">2400</b>):</span></div>
        <div className="code-line"><i>3</i><span>&nbsp;&nbsp;&nbsp;&nbsp;plan = <b className="code-blue">analyze</b>(topic, <em>"Find the key questions"</em>)</span></div>
        <div className="code-line"><i>4</i><span>&nbsp;&nbsp;&nbsp;&nbsp;answer = <b className="code-blue">analyze</b>(plan, <em>"Write a useful brief"</em>)</span></div>
        <div className="code-line"><i>5</i><span>&nbsp;&nbsp;&nbsp;&nbsp;<b className="code-purple">return</b> answer</span></div>
        <div className="code-line"><i>6</i><span>{'}'}</span></div>
      </div>
      <div className="new-landing-terminal-bar new-landing-result-bar"><span /><span /><span /><code>run complete</code></div>
      <div className="new-landing-output"><span className="new-landing-output-check"><Check size={13} /></span><span>Research brief ready</span><small>1.84s</small></div>
      <div className="new-landing-run-action"><button type="button"><CirclePlay size={14} fill="currentColor" /> Run Kora</button><span>Click to see evaluation traces</span></div>
    </div>
  )
}

export default function NewHomePage() {
  return (
    <main className="kora-new-landing">
      <header className="new-landing-nav">
        <Link className="new-landing-logo" href="/new-home"><img src="/kora-icon-s.svg" alt="" /><span>Kora</span></Link>
        <nav aria-label="Main navigation">
          <Link href="/language">Docs</Link>
          <Link href="/start-here">Guides</Link>
          <Link href="/reference">Reference</Link>
          <Link href="/ecosystem">Ecosystem</Link>
          <Link href="/roadmap">Roadmap</Link>
        </nav>
        <div className="new-landing-nav-actions">
          <Link href="/start-here">Join community <ArrowUpRight size={14} /></Link>
          <a className="new-landing-icon-link" href="https://github.com/ImAbhishekTomar/kora-lang" target="_blank" rel="noreferrer" aria-label="Kora on GitHub"><GitBranch size={16} /></a>
          <button className="new-landing-menu" type="button" aria-label="Open menu"><Menu size={18} /></button>
        </div>
      </header>

      <section className="new-landing-hero">
        <div className="new-landing-hero-copy">
          <p className="new-landing-eyebrow"><span>✦</span> Kora is a language, not a wrapper</p>
          <h1>Build AI agents<br />that keep their<br /><em>promises.</em></h1>
          <p className="new-landing-intro">A language for reliable AI workflows. Strongly typed, replayable, and safe by design - from prototype to production.</p>
          <div className="new-landing-actions"><Link className="new-landing-primary" href="/start-here">Get started <ArrowUpRight size={16} /></Link><Link className="new-landing-secondary" href="/language">Explore guides <ArrowUpRight size={16} /></Link></div>
        </div>
        <div className="new-landing-hero-demo"><CodePanel /></div>
      </section>

      <section className="new-landing-features" aria-label="Kora features">
        {features.map(({ icon: Icon, title, copy }) => <article key={title}><Icon size={21} strokeWidth={1.6} /><h2>{title}</h2><p>{copy}</p></article>)}
      </section>

      <footer className="new-landing-footer"><span><Terminal size={15} /> Kora</span><span>Agent-first, by design.</span><Link href="/">View the current docs home <ArrowUpRight size={14} /></Link></footer>
    </main>
  )
}
