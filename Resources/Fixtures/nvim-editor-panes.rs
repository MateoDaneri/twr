// Generated from clean Neovim observations by probe_editor_panes_reference.py.
const CASES: &[Case] = &[
Case { name:"vertical-horizontal-close", steps:&[
Step { keys:&["<C-w>","v"], layout:"row(1,2)", active:2, previous:1, cursors:&[(1,4,2),(2,4,2)], error:false },
Step { keys:&["<C-w>","s"], layout:"row(1,col(2,3))", active:3, previous:2, cursors:&[(1,4,2),(2,4,2),(3,4,2)], error:false },
Step { keys:&["<C-w>","c"], layout:"row(1,2)", active:2, previous:2, cursors:&[(1,4,2),(2,4,2)], error:false },
Step { keys:&["<C-w>","c"], layout:"1", active:1, previous:0, cursors:&[(1,4,2)], error:false },
Step { keys:&["<C-w>","c"], layout:"1", active:1, previous:0, cursors:&[(1,4,2)], error:true },
] },
Case { name:"row-flatten", steps:&[
Step { keys:&["<C-w>","v"], layout:"row(1,2)", active:2, previous:1, cursors:&[(1,4,2),(2,4,2)], error:false },
Step { keys:&["<C-w>","v"], layout:"row(1,2,3)", active:3, previous:2, cursors:&[(1,4,2),(2,4,2),(3,4,2)], error:false },
Step { keys:&["<C-w>","v"], layout:"row(1,2,3,4)", active:4, previous:3, cursors:&[(1,4,2),(2,4,2),(3,4,2),(4,4,2)], error:false },
Step { keys:&["<C-w>","c"], layout:"row(1,2,3)", active:3, previous:3, cursors:&[(1,4,2),(2,4,2),(3,4,2)], error:false },
Step { keys:&["<C-w>","t"], layout:"row(1,2,3)", active:1, previous:3, cursors:&[(1,4,2),(2,4,2),(3,4,2)], error:false },
Step { keys:&["<C-w>","c"], layout:"row(2,3)", active:2, previous:3, cursors:&[(2,4,2),(3,4,2)], error:false },
] },
Case { name:"mixed-first", steps:&[
Step { keys:&["<C-w>","v"], layout:"row(1,2)", active:2, previous:1, cursors:&[(1,4,2),(2,4,2)], error:false },
Step { keys:&["<C-w>","s"], layout:"row(1,col(2,3))", active:3, previous:2, cursors:&[(1,4,2),(2,4,2),(3,4,2)], error:false },
Step { keys:&["<C-w>","v"], layout:"row(1,col(2,row(3,4)))", active:4, previous:3, cursors:&[(1,4,2),(2,4,2),(3,4,2),(4,4,2)], error:false },
Step { keys:&["<C-w>","t"], layout:"row(1,col(2,row(3,4)))", active:1, previous:4, cursors:&[(1,4,2),(2,4,2),(3,4,2),(4,4,2)], error:false },
Step { keys:&["<C-w>","v"], layout:"row(1,5,col(2,row(3,4)))", active:5, previous:1, cursors:&[(1,4,2),(2,4,2),(3,4,2),(4,4,2),(5,4,2)], error:false },
] },
Case { name:"close-first", steps:&[
Step { keys:&["<C-w>","v"], layout:"row(1,2)", active:2, previous:1, cursors:&[(1,4,2),(2,4,2)], error:false },
Step { keys:&["<C-w>","s"], layout:"row(1,col(2,3))", active:3, previous:2, cursors:&[(1,4,2),(2,4,2),(3,4,2)], error:false },
Step { keys:&["<C-w>","t"], layout:"row(1,col(2,3))", active:1, previous:3, cursors:&[(1,4,2),(2,4,2),(3,4,2)], error:false },
Step { keys:&["<C-w>","c"], layout:"col(2,3)", active:2, previous:3, cursors:&[(2,4,2),(3,4,2)], error:false },
Step { keys:&["<C-w>","p"], layout:"col(2,3)", active:3, previous:2, cursors:&[(2,4,2),(3,4,2)], error:false },
] },
Case { name:"close-last", steps:&[
Step { keys:&["<C-w>","v"], layout:"row(1,2)", active:2, previous:1, cursors:&[(1,4,2),(2,4,2)], error:false },
Step { keys:&["<C-w>","s"], layout:"row(1,col(2,3))", active:3, previous:2, cursors:&[(1,4,2),(2,4,2),(3,4,2)], error:false },
Step { keys:&["<C-w>","b"], layout:"row(1,col(2,3))", active:3, previous:2, cursors:&[(1,4,2),(2,4,2),(3,4,2)], error:false },
Step { keys:&["<C-w>","c"], layout:"row(1,2)", active:2, previous:2, cursors:&[(1,4,2),(2,4,2)], error:false },
Step { keys:&["<C-w>","p"], layout:"row(1,2)", active:2, previous:2, cursors:&[(1,4,2),(2,4,2)], error:false },
] },
Case { name:"cycle-history", steps:&[
Step { keys:&["<C-w>","v"], layout:"row(1,2)", active:2, previous:1, cursors:&[(1,4,2),(2,4,2)], error:false },
Step { keys:&["<C-w>","s"], layout:"row(1,col(2,3))", active:3, previous:2, cursors:&[(1,4,2),(2,4,2),(3,4,2)], error:false },
Step { keys:&["<C-w>","t"], layout:"row(1,col(2,3))", active:1, previous:3, cursors:&[(1,4,2),(2,4,2),(3,4,2)], error:false },
Step { keys:&["<C-w>","w"], layout:"row(1,col(2,3))", active:2, previous:1, cursors:&[(1,4,2),(2,4,2),(3,4,2)], error:false },
Step { keys:&["<C-w>","W"], layout:"row(1,col(2,3))", active:1, previous:2, cursors:&[(1,4,2),(2,4,2),(3,4,2)], error:false },
Step { keys:&["<C-w>","p"], layout:"row(1,col(2,3))", active:2, previous:1, cursors:&[(1,4,2),(2,4,2),(3,4,2)], error:false },
] },
Case { name:"close-middle", steps:&[
Step { keys:&["<C-w>","v"], layout:"row(1,2)", active:2, previous:1, cursors:&[(1,4,2),(2,4,2)], error:false },
Step { keys:&["<C-w>","s"], layout:"row(1,col(2,3))", active:3, previous:2, cursors:&[(1,4,2),(2,4,2),(3,4,2)], error:false },
Step { keys:&["<C-w>","t"], layout:"row(1,col(2,3))", active:1, previous:3, cursors:&[(1,4,2),(2,4,2),(3,4,2)], error:false },
Step { keys:&["<C-w>","w"], layout:"row(1,col(2,3))", active:2, previous:1, cursors:&[(1,4,2),(2,4,2),(3,4,2)], error:false },
Step { keys:&["<C-w>","c"], layout:"row(1,3)", active:3, previous:1, cursors:&[(1,4,2),(3,4,2)], error:false },
] },
Case { name:"no-previous", steps:&[
Step { keys:&["<C-w>","p"], layout:"1", active:1, previous:0, cursors:&[(1,4,2)], error:false },
] },
Case { name:"independent-cursors", steps:&[
Step { keys:&["<C-w>","v"], layout:"row(1,2)", active:2, previous:1, cursors:&[(1,4,2),(2,4,2)], error:false },
Step { keys:&["j"], layout:"row(1,2)", active:2, previous:1, cursors:&[(1,4,2),(2,5,2)], error:false },
Step { keys:&["<C-w>","p"], layout:"row(1,2)", active:1, previous:2, cursors:&[(1,4,2),(2,5,2)], error:false },
Step { keys:&["<C-w>","p"], layout:"row(1,2)", active:2, previous:1, cursors:&[(1,4,2),(2,5,2)], error:false },
Step { keys:&["<C-w>","s"], layout:"row(1,col(2,3))", active:3, previous:2, cursors:&[(1,4,2),(2,5,2),(3,5,2)], error:false },
] },
];
