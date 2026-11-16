export function initialSkillReview(project) {
  return { project, selected: null, preview: null, confirmation: null };
}

export function skillReview(state, action) {
  if (action.type === 'project') {
    return state.project === action.project ? state : initialSkillReview(action.project);
  }
  if (action.project !== state.project) {
    return state;
  }
  switch (action.type) {
    case 'select':
      return { ...state, selected: { project: action.project, candidate: action.candidate },
        preview: null, confirmation: null };
    case 'clear':
      return initialSkillReview(state.project);
    case 'clear-preview':
      return { ...state, preview: null, confirmation: null };
    case 'preview':
      if (!state.selected || state.selected.candidate !== action.candidate) {
        return state;
      }
      return { ...state, preview: { ...action.preview, project: action.project } };
    case 'confirm':
      if (!state.preview) {
        return state;
      }
      return {
        ...state,
        confirmation: { project: state.project, kind: action.kind, preview: state.preview },
      };
    case 'close-confirmation':
      return { ...state, confirmation: null };
    case 'status':
      if (!state.preview || state.preview.candidate !== action.candidate) {
        return state;
      }
      return { ...state, preview: { ...state.preview, status: action.status } };
    default:
      return state;
  }
}

export function approveRequest(preview) {
  return {
    project: preview.project,
    candidate: preview.candidate,
    revision: preview.revision_id,
    draft_hash: preview.draft_hash,
    paths: preview.paths.map(item => item.path),
    confirm: true,
  };
}
