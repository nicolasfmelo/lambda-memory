export function initialGrid(routeKey = '') {
  return {
    routeKey,
    filters: { query: '', basis: '', updated: false, topic: '', sort: 'recent' },
    cards: [],
    cursor: null,
  };
}

export function gridReducer(state, action) {
  if (action.type === 'route') {
    return action.key === state.routeKey ? state : initialGrid(action.key);
  }
  if (action.type === 'filters') {
    const filters = { ...state.filters, ...action.values };
    if (Object.keys(action.values).every(key => filters[key] === state.filters[key])) return state;
    return { ...state, filters, cards: [], cursor: null };
  }
  if (action.type === 'filter') {
    if (state.filters[action.key] === action.value) return state;
    return { ...state, filters: { ...state.filters, [action.key]: action.value }, cards: [], cursor: null };
  }
  if (action.type === 'page') {
    const seen = new Set(state.cards.map(card => card.node));
    const cards = [...state.cards];
    for (const card of action.nodes) {
      if (!seen.has(card.node)) {
        seen.add(card.node);
        cards.push(card);
      }
    }
    return { ...state, cards, cursor: action.cursor ?? null };
  }
  return state;
}

export function flatCount(state) {
  return `${state.cards.length}${state.cursor ? '+' : ''}`;
}

export function debounceKey(query) {
  return query;
}

export function neighbor(ids, current, direction) {
  const index = ids.indexOf(current);
  return index < 0 ? null : ids[index + direction] ?? null;
}
