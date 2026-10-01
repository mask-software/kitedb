import MiniSearch from 'minisearch'
import { docsStructure, type DocPage } from './docs'

export interface SearchResult {
  id: string
  title: string
  description: string
  slug: string
  section: string
  score: number
}

// Create a flat list of all docs with section info
function getAllDocs(): Array<DocPage & { id: string; section: string }> {
  const docs: Array<DocPage & { id: string; section: string }> = []
  
  for (const section of docsStructure) {
    for (const item of section.items) {
      docs.push({
        ...item,
        id: item.slug || 'index',
        section: section.label,
      })
    }
  }
  
  return docs
}

// Initialize MiniSearch with all docs
let searchIndex: MiniSearch | null = null

export function getSearchIndex(): MiniSearch {
  if (searchIndex) return searchIndex
  
  searchIndex = new MiniSearch({
    fields: ['title', 'description', 'slug', 'section'],
    storeFields: ['title', 'description', 'slug', 'section'],
    searchOptions: {
      boost: { title: 3, description: 1.5, section: 1 },
      fuzzy: 0.2,
      prefix: true,
    },
  })
  
  const docs = getAllDocs()
  searchIndex.addAll(docs)
  
  return searchIndex
}

export function search(query: string, limit = 10): SearchResult[] {
  if (!query.trim()) return []
  
  const index = getSearchIndex()
  // MiniSearch has no limit option; results come back sorted by score
  const results = index.search(query).slice(0, limit)

  return results.map((result) => ({
    id: result.id as string,
    title: result.title as string,
    description: result.description as string,
    slug: result.slug as string,
    section: result.section as string,
    score: result.score,
  }))
}
