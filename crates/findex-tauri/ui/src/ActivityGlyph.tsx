import { useEffect, useState } from 'react';

const FRAMES = [
  ['00111100','01111110','01311310','01411410','01111110','01155110','00122100','00000000'],
  ['00111100','01111110','01311310','01411410','01155110','01111110','00122100','00000000'],
  ['00111100','01111110','01311310','01411410','01155110','01155110','00155100','00000000'],
  ['06111100','01111116','61311310','01411410','01155110','01155116','00155100','00060000'],
  ['00111100','01111110','01311310','11411411','11155111','01111110','00122100','00000000'],
  ['00177100','01711710','01311310','01411410','01155110','01777710','00177100','00000000'],
  ['00611600','00111100','01111110','01344310','01111110','01155110','00122100','00000000'],
  ['00000000','00111100','01111110','01311310','01411410','01111110','01155110','00122100']
] as const;

export default function ActivityGlyph({ active = true, label = 'Indexing code graph' }: { active?: boolean; label?: string }) {
  const [frame, setFrame] = useState(0);
  useEffect(() => {
    if (!active || window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;
    const timer = window.setInterval(() => setFrame(value => (value + 1) % FRAMES.length), 115);
    return () => window.clearInterval(timer);
  }, [active]);
  const pixels = FRAMES[frame].join('').split('');
  return <div className="activity-glyph" role="status" aria-label={label}>
    <span className="activity-pixels" aria-hidden="true">{pixels.map((pixel, index) => <i className={`p${pixel}`} key={index} />)}</span>
    <span>{label}</span>
  </div>;
}
