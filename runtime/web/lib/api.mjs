export const NOT_CONFIRMED = 'Not confirmed — check before retrying.';
export function errorText(body) {
  if (typeof body === 'string')
    return body || 'The tool returned an error.';
  if (body?.message && body instanceof Error)
    return body.message;
  if (typeof body?.error === 'string')
    return body.error +
        (body.diagnostics_warning ? ` (diagnostics: ${body.diagnostics_warning.code})` : '');
  return JSON.stringify(body) ?? 'The tool returned an error.';
}
class ApiError extends Error {
  constructor(kind, message, status, body) {
    super(message);
    this.kind = kind;
    this.status = status;
    this.body = body;
  }
}
export function createApi({fetchImpl, csrf}) {
  async function api(path, body) {
    const init = {
      method: body === undefined ? 'GET' : 'POST',
      headers: {'x-lambda-memory-csrf': csrf, 'Content-Type': 'application/json'},
      credentials: 'same-origin',
      cache: 'no-store'
    };
    if (body !== undefined)
      init.body = JSON.stringify(body);
    let response;
    try {
      response = await fetchImpl(path, init);
    } catch {
      throw new ApiError('unconfirmed', `The daemon did not answer. ${NOT_CONFIRMED}`, 0, null);
    }
    let data = null;
    try {
      data = await response.json();
    } catch {
    }
    if (response.ok)
      return data;
    const status = response.status;
    if (status === 409 && data?.needs_start)
      throw new ApiError(
          'needs_start', 'The worker for this project is not running.', status, data);
    if (status === 502 && data?.capture_saved)
      throw new ApiError(
          'unconfirmed',
          'Evidence was saved but the correction was not confirmed; check before retrying.', status, data);
    if (status === 502)
      throw new ApiError('unconfirmed', NOT_CONFIRMED, status, data);
    if (status === 503)
      throw new ApiError(
          'busy', `${data?.message ?? 'Memory service busy.'} Please retry later.`, status, data);
    if (status === 400)
      throw new ApiError(
          'invalid',
          `Invalid request: ${
              data?.error ?? 'the daemon rejected the arguments'}. Nothing was written.`,
          status, data);
    if (status === 403)
      throw new ApiError(
          'http',
          'Forbidden. Reload the page if the daemon restarted (the CSRF token changes on each start).',
          status, data);
    throw new ApiError(
        'http', data?.error ?? data?.message ?? `Request failed (HTTP ${status}).`, status, data);
  }
  async function tool(project, name, args = {}) {
    const result = await api('/api/tool', {project, name, arguments: args});
    if (result?.isError)
      throw new ApiError('tool', errorText(result.body), 200, result.body);
    return result.body;
  }
  return {api, tool};
}
