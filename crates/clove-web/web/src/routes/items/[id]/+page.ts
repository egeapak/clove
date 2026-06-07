import type { PageLoad } from './$types';

export const load: PageLoad = ({ params }) => {
  // `view` is derived live from the URL in the page component (so changing the
  // sub-tab doesn't re-run the loader), so we only need the id here.
  return { id: params.id };
};
