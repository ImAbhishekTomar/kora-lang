import nextra from 'nextra'
import { createHighlighter } from 'shiki'
import koraGrammar from './editors/vscode/syntaxes/kora.tmLanguage.json' with { type: 'json' }

const koraHighlighter = createHighlighter({
  themes: ['github-light', 'github-dark'],
  langs: [{ ...koraGrammar, name: 'kora', displayName: 'Kora', aliases: ['ko'] }]
})

const withNextra = nextra({
  search: { codeblocks: true },
  mdxOptions: {
    rehypePrettyCodeOptions: {
      getHighlighter: () => koraHighlighter
    }
  }
})

export default withNextra({
  reactStrictMode: true
})
