import { Layout, Navbar } from 'nextra-theme-docs'
import { getPageMap } from 'nextra/page-map'
import 'nextra-theme-docs/style.css'
import '../styles.css'
import { DocsVersionSelector } from '../components/docs-version-selector'

export const metadata = {
  title: { default: 'Kora', template: '%s – Kora' },
  description: 'Friendly documentation for Kora, an agent-first programming language.',
  icons: { icon: '/icon.svg' }
}

export default async function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" dir="ltr" suppressHydrationWarning>
      <body>
        <Layout
          navbar={
            <Navbar logo={<span className="brand"><span className="brand-mark">k</span> kora</span>} projectLink="https://github.com/ImAbhishekTomar/kora-lang">
              <DocsVersionSelector />
            </Navbar>
          }
          pageMap={await getPageMap()}
          docsRepositoryBase="https://github.com/ImAbhishekTomar/kora-lang/tree/main"
        >
          {children}
        </Layout>
      </body>
    </html>
  )
}
