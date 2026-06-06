import type { PageLoad } from './$types';

export const load: PageLoad = ({ params, url }) => {
  return {
    id: params.id,
    view: url.searchParams.get('view') ?? 'overview'
  };
};
