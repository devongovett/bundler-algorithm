#![feature(hash_drain_filter)]
#![feature(drain_filter)]

extern crate petgraph;

use std::collections::{HashSet, HashMap, LinkedList};
use petgraph::Graph;
use petgraph::prelude::{NodeIndex, Incoming};
use petgraph::visit::{depth_first_search, DfsEvent, Control};
use petgraph::dot::{Dot};

#[derive(Debug, PartialEq, Eq, Hash)]
enum AssetType {
  JavaScript,
  CSS,
  HTML
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct Asset<'a> {
  name: &'a str,
  asset_type: AssetType,
  size: usize
}

#[derive(Debug)]
struct Dependency {
  is_async: bool
}

#[derive(Debug, Default)]
struct Bundle {
  asset_ids: Vec<NodeIndex>,
  size: usize,
  source_bundles: Vec<NodeIndex>
}

impl Bundle {
  fn from_asset(asset_id: NodeIndex, asset: &Asset) -> Self {
    Bundle {
      asset_ids: vec![asset_id],
      size: asset.size,
      source_bundles: vec![]
    }
  }
}

fn main() {
  let (g, entries) = build_graph();
  println!("{:?}", Dot::new(&g));

  let mut bundle_roots = HashMap::new();
  let mut reachable_bundles = HashSet::new();
  let mut bundle_graph = Graph::new();

  // Step 1: Create bundles at the explicit split points in the graph.
  // Create bundles for each entry.
  for entry in &entries {
    let bundle_id = bundle_graph.add_node(Bundle::from_asset(*entry, &g[*entry]));
    bundle_roots.insert(*entry, (bundle_id, bundle_id));
  }
  
  // Traverse the asset graph and create bundles for asset type changes and async dependencies.
  // This only adds the entry asset of each bundle, not the subgraph.
  let mut stack = LinkedList::new();
  depth_first_search(&g, entries, |event| {
    match event {
      DfsEvent::Discover(asset_id, _) => {
        // Push to the stack when a new bundle is created.
        if let Some((_, bundle_group_id)) = bundle_roots.get(&asset_id) {
          stack.push_front((asset_id, *bundle_group_id));
        }
      }
      DfsEvent::TreeEdge(u, v) => {
        let asset_a = &g[u];
        let asset_b = &g[v];

        // Create a new bundle when the asset type changes.
        if asset_a.asset_type != asset_b.asset_type {
          let (_, bundle_group_id) = stack.front().unwrap();
          let bundle_id = bundle_graph.add_node(Bundle::from_asset(v, &g[v]));
          bundle_roots.insert(v, (bundle_id, *bundle_group_id));

          // Add an edge from the bundle group entry to the new bundle.
          // This indicates that the bundle is loaded together with the entry.
          bundle_graph.add_edge(*bundle_group_id, bundle_id, 0);
          return
        }

        // Create a new bundle as well as a new bundle group if the dependency is async.
        let dependency = &g[g.find_edge(u, v).unwrap()];
        if dependency.is_async {
          let bundle_id = bundle_graph.add_node(Bundle::from_asset(v, &g[v]));
          bundle_roots.insert(v, (bundle_id, bundle_id));

          // Walk up the stack until we hit a different asset type
          // and mark each this bundle as reachable from every parent bundle.
          for (b, _) in &stack {
            let a = &g[*b];
            if a.asset_type != asset_b.asset_type {
              break
            }
            reachable_bundles.insert((*b, v));
          }
        }
      }
      DfsEvent::Finish(n, _) => {
        // Pop the stack when existing the asset node that created a bundle.
        if let Some((s, _)) = stack.front() {
          if *s == n {
            stack.pop_front();
          }
        }
      }
      _ => {}
    }
  });

  println!("roots {:?}", bundle_roots);
  println!("reachable {:?}", reachable_bundles);
  println!("initial bundle graph {:?}", Dot::new(&bundle_graph));

  // Step 2: Determine reachability for every asset from each bundle root.
  // This is later used to determine which bundles to place each asset in.
  let mut reachable_nodes = HashSet::new();
  for (root, _) in &bundle_roots {
    depth_first_search(&g, Some(*root), |event| {
      if let DfsEvent::Discover(n, _) = &event {
        if n == root {
          return Control::Continue
        }

        // Stop when we hit another bundle root.
        if bundle_roots.contains_key(&n) {
          return Control::<()>::Prune;
        }

        reachable_nodes.insert((*root, *n));
      }
      Control::Continue
    });
  }

  let reachable_graph = Graph::<(), ()>::from_edges(&reachable_nodes);
  println!("{:?}", Dot::new(&reachable_graph));

  // Step 3: Place all assets into bundles. Each asset is placed into a single
  // bundle based on the bundle entries it is reachable from. This creates a
  // maximally code split bundle graph with no duplication.

  // Create a mapping from entry asset ids to bundle ids.
  let mut bundles: HashMap<Vec<NodeIndex>, NodeIndex> = HashMap::new();

  for asset_id in g.node_indices() {
    // Find bundle entries reachable from the asset.
    let reachable: Vec<NodeIndex> = reachable_graph.neighbors_directed(asset_id, Incoming).collect();

    // Filter out bundles when the asset is reachable in a parent bundle.
    let reachable: Vec<NodeIndex> = reachable.iter().cloned().filter(|b| {
      (&reachable).into_iter().all(|a| !reachable_bundles.contains(&(*a, *b)))
    }).collect();

    if let Some((bundle_id, _)) = bundle_roots.get(&asset_id) {
      // If the asset is a bundle root, add the bundle to every other reachable bundle group.
      bundles.entry(vec![asset_id]).or_insert(*bundle_id);
      for a in &reachable {
        if *a != asset_id {
          bundle_graph.add_edge(bundle_roots[a].1, *bundle_id, 0);
        }
      }
    } else if reachable.len() > 0 {
      // If the asset is reachable from more than one entry, find or create
      // a bundle for that combination of entries, and add the asset to it.
      let source_bundles = reachable.iter().map(|a| bundles[&vec![*a]]).collect();
      let bundle_id = bundles.entry(reachable.clone()).or_insert_with(|| {
        let mut bundle = Bundle::default();
        bundle.source_bundles = source_bundles;
        bundle_graph.add_node(bundle)
      });

      let bundle = &mut bundle_graph[*bundle_id];
      bundle.asset_ids.push(asset_id);
      bundle.size += g[asset_id].size;

      // Add the bundle to each reachable bundle group.
      for a in reachable {
        if a != *bundle_id {
          bundle_graph.add_edge(bundle_roots[&a].1, *bundle_id, 0);
        }
      }
    }
  }

  // Step 4: Remove shared bundles that are smaller than the minimum size,
  // and add the assets to the original source bundles they were referenced from.
  // This may result in duplication of assets in multiple bundles.
  for bundle_id in bundle_graph.node_indices() {
    let bundle = &bundle_graph[bundle_id];
    if bundle.source_bundles.len() > 0 && bundle.size < 10 {
      remove_bundle(&g, &mut bundle_graph, bundle_id);
    }
  }

  // Step 5: Remove shared bundles from bundle groups that hit the parallel request limit.
  let limit = 3;
  for (_, (bundle_id, bundle_group_id)) in bundle_roots {
    // Only handle bundle group entries.
    if bundle_id != bundle_group_id {
      continue;
    }

    // Find the bundles in this bundle group.
    let mut neighbors: Vec<NodeIndex> = bundle_graph.neighbors(bundle_group_id).collect();
    if neighbors.len() > limit {
      // Sort the bundles so the smallest ones are removed first.
      neighbors.sort_by(|a, b| bundle_graph[*a].size.cmp(&bundle_graph[*b].size));

      // Remove bundles until the bundle group is within the parallel request limit.
      for bundle_id in &neighbors[0..neighbors.len() - limit] {
        // Add all assets in the shared bundle into the source bundles that are within this bundle group.
        let source_bundles: Vec<NodeIndex> = bundle_graph[*bundle_id].source_bundles.drain_filter(|s| neighbors.contains(s)).collect();
        for source in source_bundles {
          for asset_id in bundle_graph[*bundle_id].asset_ids.clone() {
            let bundle_id = bundles[&vec![source]];
            let bundle = &mut bundle_graph[bundle_id];
            bundle.asset_ids.push(asset_id);
            bundle.size += g[asset_id].size;
          }
        }

        // Remove the edge from this bundle group to the shared bundle.
        bundle_graph.remove_edge(bundle_graph.find_edge(bundle_group_id, *bundle_id).unwrap());

        // If there is now only a single bundle group that contains this bundle,
        // merge it into the remaining source bundles. If it is orphaned entirely, remove it.
        let count = bundle_graph.neighbors_directed(*bundle_id, Incoming).count();
        if count == 1 {
          remove_bundle(&g, &mut bundle_graph, *bundle_id);
        } else if count == 0 {
          bundle_graph.remove_node(*bundle_id);
        }
      }
    }
  }

  println!("bundle graph {:?}", Dot::new(&bundle_graph));

  for bundle_id in bundle_graph.node_indices() {
    let bundle = &bundle_graph[bundle_id];
    println!("{} {}", bundle.asset_ids.iter().map(|n| g[*n].name).collect::<Vec<&str>>().join(", "), bundle.size)
  }
}

fn remove_bundle(
  asset_graph: &Graph<Asset, Dependency>,
  bundle_graph: &mut Graph<Bundle, i32>,
  bundle_id: NodeIndex
) {
  let bundle = bundle_graph.remove_node(bundle_id).unwrap();
  for asset_id in &bundle.asset_ids {
    for source_bundle_id in &bundle.source_bundles {
      let bundle = &mut bundle_graph[*source_bundle_id];
      bundle.asset_ids.push(*asset_id);
      bundle.size += asset_graph[*asset_id].size;
    }
  }
}

fn build_graph<'a>() -> (Graph<Asset<'a>, Dependency>, Vec<NodeIndex>) {
  let mut g = Graph::new();
  let mut entries = Vec::new();

  let html = g.add_node(Asset {
    name: "a.html",
    asset_type: AssetType::HTML,
    size: 10
  });

  let html2 = g.add_node(Asset {
    name: "b.html",
    asset_type: AssetType::HTML,
    size: 10
  });

  let js = g.add_node(Asset {
    name: "a.js",
    asset_type: AssetType::JavaScript,
    size: 10
  });

  let js2 = g.add_node(Asset {
    name: "async.js",
    asset_type: AssetType::JavaScript,
    size: 10
  });

  let js3 = g.add_node(Asset {
    name: "async2.js",
    asset_type: AssetType::JavaScript,
    size: 10
  });

  let js4 = g.add_node(Asset {
    name: "b.js",
    asset_type: AssetType::JavaScript,
    size: 10
  });

  let js5 = g.add_node(Asset {
    name: "shared.js",
    asset_type: AssetType::JavaScript,
    size: 10
  });

  let css = g.add_node(Asset {
    name: "styles.css",
    asset_type: AssetType::CSS,
    size: 10
  });

  g.add_edge(html, js, Dependency {
    is_async: false
  });
  g.add_edge(js, js2, Dependency {
    is_async: true
  });
  g.add_edge(js, js3, Dependency {
    is_async: false
  });
  g.add_edge(js2, js3, Dependency {
    is_async: false
  });
  g.add_edge(js3, js5, Dependency {
    is_async: false
  });
  g.add_edge(js, css, Dependency {
    is_async: false
  });

  g.add_edge(html2, js4, Dependency {
    is_async: false
  });

  g.add_edge(js4, js5, Dependency {
    is_async: false
  });
  
  entries.push(html);
  entries.push(html2);

  return (g, entries);
}
