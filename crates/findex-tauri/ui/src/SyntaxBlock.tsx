import hljs from 'highlight.js/lib/core';
import bash from 'highlight.js/lib/languages/bash';
import c from 'highlight.js/lib/languages/c';
import cpp from 'highlight.js/lib/languages/cpp';
import csharp from 'highlight.js/lib/languages/csharp';
import css from 'highlight.js/lib/languages/css';
import dart from 'highlight.js/lib/languages/dart';
import go from 'highlight.js/lib/languages/go';
import java from 'highlight.js/lib/languages/java';
import javascript from 'highlight.js/lib/languages/javascript';
import json from 'highlight.js/lib/languages/json';
import markdown from 'highlight.js/lib/languages/markdown';
import python from 'highlight.js/lib/languages/python';
import rust from 'highlight.js/lib/languages/rust';
import typescript from 'highlight.js/lib/languages/typescript';
import xml from 'highlight.js/lib/languages/xml';
import yaml from 'highlight.js/lib/languages/yaml';

const LANGUAGES = { bash, c, cpp, csharp, css, dart, go, java, javascript, json, markdown, python, rust, typescript, xml, yaml };
for (const [name, grammar] of Object.entries(LANGUAGES)) hljs.registerLanguage(name, grammar);

const EXTENSIONS: Record<string, keyof typeof LANGUAGES> = {
  c: 'c', h: 'c', cc: 'cpp', cpp: 'cpp', cxx: 'cpp', hpp: 'cpp', cs: 'csharp', css: 'css',
  dart: 'dart', go: 'go', html: 'xml', htm: 'xml', java: 'java', js: 'javascript', jsx: 'javascript',
  json: 'json', md: 'markdown', mjs: 'javascript', py: 'python', rs: 'rust', sh: 'bash',
  ts: 'typescript', tsx: 'typescript', vue: 'xml', xml: 'xml', yaml: 'yaml', yml: 'yaml'
};

export default function SyntaxBlock({ source, path, startLine = 1 }: { source: string; path: string; startLine?: number }) {
  const extension = path.split('.').pop()?.toLowerCase() ?? '';
  const language = EXTENSIONS[extension];
  const html = language
    ? hljs.highlight(source, { language, ignoreIllegals: true }).value
    : hljs.highlightAuto(source, Object.keys(LANGUAGES)).value;
  const lineCount = Math.max(1, source.replace(/\n$/, '').split('\n').length);
  const numbers = Array.from({ length: lineCount }, (_, index) => startLine + index).join('\n');

  return <div className="source-code" data-language={language ?? 'text'}>
    <pre className="source-lines" aria-hidden="true">{numbers}</pre>
    <pre className="source-highlight"><code dangerouslySetInnerHTML={{ __html: html }} /></pre>
  </div>;
}
