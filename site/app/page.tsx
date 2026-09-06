import HomeContent from './home-content'

const repositoryApiUrl = 'https://api.github.com/repos/ImAbhishekTomar/kora-lang'

async function getStarCount() {
  try {
    const response = await fetch(repositoryApiUrl, {
      headers: {
        Accept: 'application/vnd.github+json',
        'X-GitHub-Api-Version': '2022-11-28'
      },
      next: { revalidate: 3600 }
    })

    if (!response.ok) return 0

    const data: unknown = await response.json()
    if (!data || typeof data !== 'object' || !('stargazers_count' in data)) return 0

    const count = data.stargazers_count
    return typeof count === 'number' && Number.isFinite(count) ? count : 0
  } catch {
    return 0
  }
}

export default async function HomePage() {
  return <HomeContent starCount={await getStarCount()} />
}
