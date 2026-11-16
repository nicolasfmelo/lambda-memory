export function needsYou(overview, status) {
  return [
    { label: 'Skill drafts', count: overview.skill_drafts?.drafted || 0, route: { page: 'skills' } },
    {
      label: 'Conflicts',
      count: status.review?.unresolved_conflicts || 0,
      route: { page: 'review', state: 'conflicted' },
    },
    {
      label: 'Processing',
      count: status.lifecycle?.processing || 0,
      route: { page: 'review', state: 'processing' },
    },
    { label: 'Failed forwards', count: status.forwards_failed || 0, route: { page: 'activity' } },
  ];
}

export function chartData(days) {
  const max = Math.max(0, ...days.map(d => Math.max(d.captured, d.published)));
  const format = day => new Date(`${day}T12:00:00`).toLocaleDateString('en-GB', {
    day: 'numeric',
    month: 'short',
  });
  return {
    captured: days.reduce((sum, d) => sum + d.captured, 0),
    published: days.reduce((sum, d) => sum + d.published, 0),
    max,
    days: days.map(d => ({
      ...d,
      label: format(d.day),
      tooltip: `${format(d.day)} · ${d.captured} captured · ${d.published} published`,
      capturedHeight: d.captured / (max || 1) * 100,
      publishedHeight: d.published / (max || 1) * 100,
    })),
  };
}
