/** Routing descriptors need no copy of runtime body validators. */
export function httpDescriptors(contract) {
  const ids = new Set();
  const routes = new Set();
  return Object.fromEntries(contract.operations.map((operation) => {
    const { operationId, body: _body, responses, ...descriptor } = operation;
    if (!operationId || ids.has(operationId)) {
      throw new Error(`Duplicate or missing HTTP operation ID: ${operationId}`);
    }
    const route = `${operation.method} ${operation.path.replace(/\{[^}]+\}/g, '{}')}`;
    if (routes.has(route)) throw new Error(`Duplicate HTTP route: ${route}`);
    ids.add(operationId);
    routes.add(route);
    return [operationId, {
      ...descriptor,
      operationId,
      responses: Object.fromEntries(Object.entries(responses).map(([status, response]) => [
        status, { contentTypes: Object.keys(response.content ?? {}) },
      ])),
    }];
  }));
}
