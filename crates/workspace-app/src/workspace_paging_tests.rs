use super::*;
use serde::Deserialize;
#[derive(Deserialize)]
struct Reference { cases: Vec<Case> }
#[derive(Deserialize)]
struct Case { label:String, initial:State, keys:Vec<String>, observations:Vec<State> }
#[derive(Deserialize)]
struct State { top:usize, height:usize, scrolloff:usize, selected:usize, query:String, cursor:usize, mode:String, focus:String, marks:usize }
#[test]
fn half_pages_replay_actual_snacks_in_input_insert_normal_and_list() {
    let reference:Reference=serde_json::from_str(include_str!("../../../Resources/Fixtures/snacks-paging-reference.json")).unwrap();
    let mut observed=0;
    for case in reference.cases {
        let mut w=model(); w.open_locations(PickerKind::Buffers,(0..60).map(|i|Choice{label:format!("row item {i:02}"),detail:String::new(),value:format!("/example/demo/{i}.txt")}).collect());
        {let p=w.picker.as_mut().unwrap();p.query="row".into();p.query_cursor=3;p.results=(0..60).collect();}
        let mut top=case.initial.top;
        for (key,s) in case.keys.iter().zip(case.observations) {
            if matches!(key.as_str(),"<C-d>"|"<C-u>") {
                top=w.half_page(key=="<C-d>",top,s.height,s.scrolloff).unwrap();
                assert_eq!(top,s.top,"{} {key}",case.label);
            } else { w.key(key,key.as_bytes()); top=s.top; }
            let p=w.picker.as_ref().unwrap();
            assert_eq!(p.selected,s.selected,"{} {key}",case.label);
            assert_eq!(p.query,s.query);assert_eq!(p.query_cursor,s.cursor);
            assert_eq!(p.insert,s.mode=="i");assert_eq!(p.focus==PickerFocus::Input,s.focus=="input");assert_eq!(p.marked.len(),s.marks);
            observed+=1;
        }
    }
    assert_eq!(observed,18);
}
#[test]
fn tree_half_pages_use_visible_rows_keep_range_and_sticky_count() {
    let mut w=model(); w.focus(Surface::Tree);
    let root=w.projects[w.project].path.clone();
    w.entries=(0..100).map(|i|Entry{path:root.join(format!("{i:03}.rs")),label:format!("{i:03}.rs"),directory:false,depth:0}).collect();
    w.tree_selected=0;w.tree_anchor=Some(w.entries[0].path.clone());
    assert_eq!(w.half_page(true,0,20,4),Some(10));assert_eq!(w.tree_selected,10);assert_eq!(w.tree_range().len(),11);
    w.key("3",b"3");assert_eq!(w.half_page(true,10,20,4),Some(13));assert_eq!(w.tree_selected,13);
    assert_eq!(w.half_page(false,13,20,4),Some(10));assert_eq!(w.tree_selected,10);assert_eq!(w.count,0);
    for _ in 0..40 { w.half_page(true,80,20,4); }
    assert_eq!(w.tree_selected,99);assert!(w.take_tree_scroll_request());
}
#[test]
fn agents_and_backlog_page_visible_rows_and_keep_selection_identity() {
    let mut w=model();
    let seed=w.overview.agents()[0].clone();
    let agents:Vec<_>=(0..40).map(|index| {let mut agent=seed.clone();agent.id=format!("page-{index:02}");agent.label=agent.id.clone();agent}).collect();
    w.overview.replace(agents,1000).unwrap();
    for surface in [Surface::Agents,Surface::Backlog] {
        w.overview.view.task_locations.clear();
        w.overview.view.later_collapsed=false;
        if surface==Surface::Backlog {
            for index in 0..40 {w.overview.view.task_locations.insert(format!("page-{index:02}"),Location::Later);}
        }
        w.focus(surface);
        let loc=if surface==Surface::Agents {Location::Main}else{Location::Later};
        let rows:Vec<_>=w.rows(loc).into_iter().filter(|(r,_)|!matches!(r,AgentRow::Backlog)).collect();
        assert!(rows.len()>=40);
        w.selected=rows[0].0.clone();
        let mut top=w.half_page(true,0,10,4).unwrap();
        assert_eq!(w.selected,rows[5].0);
        for _ in 0..20 {top=w.half_page(true,top,10,4).unwrap();}
        assert_eq!(w.selected,rows.last().unwrap().0);
        assert_eq!(top,rows.len()-10);
        let previous=w.selected.clone();
        w.half_page(false,top,10,4);
        assert_ne!(w.selected,previous);
        if surface==Surface::Agents {assert_eq!(w.selected,w.main_selection);}else{assert_eq!(w.selected,w.backlog_selection);}
        assert!(w.take_agent_scroll_request());
    }
}
#[test]
fn paging_never_owns_terminal_insert_editor_or_details_model_keys() {
    let mut w=model();
    for surface in [Surface::Terminal,Surface::Normal,Surface::File,Surface::Details] {
        w.focus(surface);assert_eq!(w.half_page(true,0,20,4),None);
    }
    w.focus(Surface::Terminal);
    assert_eq!(w.key("<C-d>",b"\x04"),Effect::Pty(vec![4]));
    assert_eq!(w.key("<C-u>",b"\x15"),Effect::Pty(vec![21]));
    w.focus(Surface::Normal);
    assert_eq!(w.key("<C-d>",b"\x04"),Effect::TerminalMotion("<C-d>".into(),1));
    assert_eq!(w.key("<C-u>",b"\x15"),Effect::TerminalMotion("<C-u>".into(),1));
}
