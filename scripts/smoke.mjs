// Live acceptance test. Requires Node 18+ and a running, authenticated gprox.
// Four tiny generations consume the current Codex subscription allowance.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';

const binary = process.env.GPROX_BIN || 'gprox';
const args = process.env.GPROX_HOME ? ['--home', process.env.GPROX_HOME, 'key'] : ['key'];
const key = execFileSync(binary, args, { encoding: 'utf8', windowsHide: true }).trim();
const base = process.env.GPROX_BASE_URL || 'http://127.0.0.1:8787/v1';
const model = process.env.GPROX_MODEL || 'gpt-5.5';
const headers = { authorization: `Bearer ${key}`, 'content-type': 'application/json' };
assert.equal((await fetch(`${base}/models`)).status, 401);
const models = await (await fetch(`${base}/models`, { headers })).json();
assert(models.data.some(item => item.id === model), `Model missing: ${model}`);
console.log(`Models and authentication: OK (${model})`);

for (const endpoint of ['chat/completions', 'responses']) {
  for (const stream of [false, true]) {
    const body = endpoint === 'responses'
      ? { model, input: 'Reply with exactly GPROX_OK', reasoning: { effort: 'low' }, stream, store: false }
      : { model, messages: [{ role: 'user', content: 'Reply with exactly GPROX_OK' }], reasoning_effort: 'low', stream };
    const response = await fetch(`${base}/${endpoint}`, {
      method: 'POST', headers, body: JSON.stringify(body), signal: AbortSignal.timeout(120000),
    });
    assert.equal(response.status, 200, await (response.status === 200 ? Promise.resolve('') : response.text()));
    if (!stream) {
      const value = await response.json();
      const text = endpoint === 'responses' ? value.output[0].content[0].text : value.choices[0].message.content;
      assert.equal(text.trim(), 'GPROX_OK');
      assert(value.usage.total_tokens > 0);
    } else {
      assert(response.headers.get('content-type').startsWith('text/event-stream'));
      const text = await response.text();
      const events = text.split('\n').filter(line => line.startsWith('data: ')).map(line => line.slice(6));
      let output = '';
      for (const data of events.filter(data => data !== '[DONE]')) {
        const event = JSON.parse(data);
        assert(!event.error, JSON.stringify(event.error));
        assert(event.type !== 'response.failed', JSON.stringify(event));
        if (endpoint === 'responses') {
          if (event.type === 'response.output_text.delta') output += event.delta;
        } else output += event.choices?.[0]?.delta?.content || '';
      }
      assert.equal(output.trim(), 'GPROX_OK');
      assert(endpoint === 'responses' ? text.includes('event: response.completed') : events.includes('[DONE]'));
    }
    console.log(`${endpoint} ${stream ? 'SSE' : 'JSON'}: OK`);
  }
}
