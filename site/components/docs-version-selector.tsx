'use client'

import { useState } from 'react'

const versions = [
  { label: '0.2.0 (latest)', href: '/' },
  { label: '0.1.0', href: 'https://github.com/ImAbhishekTomar/kora-lang/tree/v0.1.0/docs' },
  { label: '0.0.2', href: 'https://github.com/ImAbhishekTomar/kora-lang/tree/v0.0.2/docs' },
  { label: '0.0.1', href: 'https://github.com/ImAbhishekTomar/kora-lang/blob/v0.0.1/README.md' }
]

export function DocsVersionSelector() {
  const [selectedVersion, setSelectedVersion] = useState('/')

  return (
    <label className="version-selector">
      <span className="sr-only">Documentation version</span>
      <select
        aria-label="Documentation version"
        value={selectedVersion}
        onChange={(event) => {
          const href = event.target.value
          setSelectedVersion(href)
          window.location.assign(href)
        }}
      >
        {versions.map((version) => (
          <option key={version.label} value={version.href}>
            {version.label}
          </option>
        ))}
      </select>
    </label>
  )
}
