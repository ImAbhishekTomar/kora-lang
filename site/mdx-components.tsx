import { useMDXComponents as getThemeComponents } from 'nextra-theme-docs'
import type { MDXComponents } from 'nextra/mdx-components'

export function useMDXComponents(components?: MDXComponents) {
  return components ? getThemeComponents(components) : getThemeComponents()
}
