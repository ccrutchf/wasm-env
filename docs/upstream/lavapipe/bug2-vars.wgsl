@group(0) @binding(0) var<storage, read> args: array<u32>;
@group(0) @binding(1) var<storage, read_write> buf0: array<u32>;
@group(0) @binding(2) var<storage, read_write> buf1: array<u32>;
@group(0) @binding(3) var<storage, read_write> buf2: array<u32>;
var<private> glob0: array<u32, 130> = array<u32, 130>(0u, 0u, 0u, 1066385576u, 0u, 1067418264u, 0u, 1067938904u, 0u, 1068436016u, 0u, 1068712407u, 0u, 1068953810u, 0u, 1069191789u, 0u, 1069426439u, 0u, 1069602686u, 2147483648u, 1069716818u, 0u, 1069829419u, 0u, 1069940528u, 0u, 1070050185u, 2147483648u, 1070158426u, 0u, 1070265289u, 2147483648u, 1070370807u, 0u, 1070475015u, 0u, 1070577944u, 3221225472u, 1070637860u, 2147483648u, 1070688092u, 3221225472u, 1070737729u, 2147483648u, 1070786786u, 1073741824u, 1070835276u, 3221225472u, 1070883211u, 2147483648u, 1070930605u, 2147483648u, 1070977469u, 0u, 1071023816u, 3221225472u, 1071069655u, 0u, 1071115000u, 2147483648u, 1071159859u, 0u, 1071204244u, 3221225472u, 1071248163u, 2147483648u, 1071291628u, 1073741824u, 1071334647u, 2147483648u, 1071377229u, 3221225472u, 1071419383u, 1073741824u, 1071461118u, 0u, 1071502442u, 1073741824u, 1071543362u, 1073741824u, 1071583887u, 2147483648u, 1071624024u, 2684354560u, 1071654226u, 1073741824u, 1071673918u, 3758096384u, 1071693426u, 2147483648u, 1071712755u, 3221225472u, 1071731907u, 2684354560u, 1071750886u, 1073741824u, 1071769695u, 3221225472u, 1071788336u, 0u, 1071806814u, 3758096384u, 1071825129u, 536870912u, 1071843287u, 2147483648u, 1071861288u, 3221225472u, 1071879136u, 1610612736u, 1071896834u, 3758096384u, 1071914383u, 3221225472u, 1071931787u, 1073741824u, 1071949048u, 0u, 1071966168u, 536870912u, 1071983149u, 3221225472u, 1071999993u, 1073741824u, 1072016704u, 2684354560u, 1072033282u, 3758096384u, 1072049730u);
var<private> glob1: array<u32, 130> = array<u32, 130>(0u, 0u, 2088367067u, 1043734400u, 2550294472u, 1047032769u, 2676541086u, 1046398555u, 1422667673u, 1045955116u, 974279472u, 1046659962u, 1769176211u, 1046568109u, 2981266921u, 1045243242u, 3111811746u, 1046186684u, 2522457564u, 1047475235u, 1221788023u, 1047398397u, 1201272054u, 1047212767u, 4109507832u, 1047276004u, 1222581081u, 1041270472u, 3027648816u, 1045230678u, 4239799192u, 1046716463u, 2288716150u, 1046603430u, 3111811746u, 1047235260u, 2325960018u, 1047222159u, 1907341820u, 1046559176u, 117558884u, 1046861267u, 3000049986u, 1047390140u, 2555254372u, 1047359630u, 3922145078u, 1045784901u, 333994970u, 1042342646u, 3022464246u, 1044166624u, 3844622023u, 1047505924u, 2932097726u, 1046387476u, 55472422u, 1047223784u, 3786562003u, 1047380311u, 1792449634u, 1044055305u, 4206513373u, 1046225174u, 637098823u, 1046896588u, 1233995856u, 1046125064u, 2974179191u, 1047389767u, 2079819293u, 1046325019u, 2259720032u, 1044277864u, 3937464164u, 1047462005u, 2197526683u, 1043531538u, 3287400146u, 1046539075u, 3971177604u, 1046458118u, 1035165544u, 1045361126u, 4212814245u, 1044813635u, 1078876054u, 1047272671u, 182084190u, 1044419754u, 2533221356u, 1047407948u, 3920203572u, 1046672597u, 1858886846u, 1046767561u, 3493719130u, 1047425088u, 1115872822u, 1046986431u, 451639359u, 1046645169u, 393409848u, 1046120733u, 3189749137u, 1046695696u, 3541494675u, 1047370977u, 430556226u, 1046697514u, 4115138636u, 1045750445u, 1556662650u, 1045466341u, 333994970u, 1043391222u, 3137480639u, 1047375934u, 2963058840u, 1046591323u, 2881085218u, 1044999958u, 2571181154u, 1046852595u, 1509315413u, 1045982014u, 3940658194u, 1045579820u, 4013259068u, 1047460409u);
var<private> glob2: array<u32, 482> = array<u32, 482>(0u, 1068496219u, 0u, 1068562846u, 0u, 1068628077u, 0u, 1068693273u, 0u, 1068758431u, 0u, 1068823549u, 0u, 1068888626u, 0u, 1068953659u, 0u, 1069018648u, 0u, 1069083589u, 0u, 1069148481u, 0u, 1069213322u, 0u, 1069278110u, 0u, 1069342844u, 0u, 1069407521u, 0u, 1069472140u, 0u, 1069536698u, 0u, 1069574357u, 0u, 1069606573u, 0u, 1069638757u, 0u, 1069670906u, 0u, 1069703022u, 0u, 1069735101u, 0u, 1069767144u, 0u, 1069799150u, 0u, 1069831118u, 0u, 1069863047u, 0u, 1069894936u, 0u, 1069926785u, 0u, 1069958591u, 0u, 1069990356u, 0u, 1070022077u, 0u, 1070053755u, 0u, 1070085387u, 0u, 1070116974u, 0u, 1070148515u, 0u, 1070180008u, 0u, 1070211454u, 0u, 1070242851u, 0u, 1070274198u, 0u, 1070305495u, 0u, 1070336741u, 0u, 1070367935u, 0u, 1070399077u, 0u, 1070430165u, 0u, 1070461200u, 0u, 1070492180u, 0u, 1070523105u, 0u, 1070553973u, 0u, 1070584785u, 0u, 1070605818u, 0u, 1070621166u, 0u, 1070636485u, 0u, 1070651774u, 0u, 1070667034u, 0u, 1070682263u, 0u, 1070697461u, 0u, 1070712629u, 0u, 1070727765u, 0u, 1070742870u, 0u, 1070757943u, 0u, 1070772983u, 0u, 1070787992u, 0u, 1070802968u, 0u, 1070817911u, 0u, 1070832820u, 0u, 1070847697u, 0u, 1070862539u, 0u, 1070877348u, 0u, 1070892122u, 0u, 1070906862u, 0u, 1070921567u, 0u, 1070936237u, 0u, 1070950872u, 0u, 1070965472u, 0u, 1070980035u, 0u, 1070994564u, 0u, 1071009055u, 0u, 1071023511u, 0u, 1071037930u, 0u, 1071052313u, 0u, 1071066658u, 0u, 1071080967u, 0u, 1071095238u, 0u, 1071109472u, 0u, 1071123668u, 0u, 1071137826u, 0u, 1071151947u, 0u, 1071166029u, 0u, 1071180073u, 0u, 1071194078u, 0u, 1071208045u, 0u, 1071221973u, 0u, 1071235862u, 0u, 1071249712u, 0u, 1071263523u, 0u, 1071277294u, 0u, 1071291026u, 0u, 1071304719u, 0u, 1071318372u, 0u, 1071331985u, 0u, 1071345558u, 0u, 1071359091u, 0u, 1071372584u, 0u, 1071386036u, 0u, 1071399449u, 0u, 1071412821u, 0u, 1071426152u, 0u, 1071439443u, 0u, 1071452693u, 0u, 1071465902u, 0u, 1071479071u, 0u, 1071492199u, 0u, 1071505285u, 0u, 1071518331u, 0u, 1071531336u, 0u, 1071544299u, 0u, 1071557221u, 0u, 1071570102u, 0u, 1071582942u, 0u, 1071595741u, 0u, 1071608498u, 0u, 1071621213u, 0u, 1071633888u, 0u, 1071645596u, 0u, 1071651892u, 0u, 1071658167u, 0u, 1071664421u, 0u, 1071670654u, 0u, 1071676867u, 0u, 1071683059u, 0u, 1071689230u, 0u, 1071695380u, 0u, 1071701509u, 0u, 1071707618u, 0u, 1071713706u, 0u, 1071719773u, 0u, 1071725819u, 0u, 1071731845u, 0u, 1071737850u, 0u, 1071743834u, 0u, 1071749797u, 0u, 1071755740u, 0u, 1071761662u, 0u, 1071767563u, 0u, 1071773444u, 0u, 1071779304u, 0u, 1071785143u, 0u, 1071790961u, 0u, 1071796759u, 0u, 1071802537u, 0u, 1071808294u, 0u, 1071814030u, 0u, 1071819746u, 0u, 1071825441u, 0u, 1071831116u, 0u, 1071836770u, 0u, 1071842404u, 0u, 1071848018u, 0u, 1071853611u, 0u, 1071859184u, 0u, 1071864736u, 0u, 1071870269u, 0u, 1071875781u, 0u, 1071881273u, 0u, 1071886744u, 0u, 1071892196u, 0u, 1071897628u, 0u, 1071903039u, 0u, 1071908431u, 0u, 1071913802u, 0u, 1071919154u, 0u, 1071924486u, 0u, 1071929798u, 0u, 1071935090u, 0u, 1071940362u, 0u, 1071945615u, 0u, 1071950848u, 0u, 1071956062u, 0u, 1071961256u, 0u, 1071966430u, 0u, 1071971585u, 0u, 1071976720u, 0u, 1071981836u, 0u, 1071986933u, 0u, 1071992010u, 0u, 1071997069u, 0u, 1072002108u, 0u, 1072007128u, 0u, 1072012128u, 0u, 1072017110u, 0u, 1072022073u, 0u, 1072027017u, 0u, 1072031942u, 0u, 1072036848u, 0u, 1072041736u, 0u, 1072046605u, 0u, 1072051455u, 0u, 1072056286u, 0u, 1072061099u, 0u, 1072065894u, 0u, 1072070670u, 0u, 1072075428u, 0u, 1072080167u, 0u, 1072084888u, 0u, 1072089591u, 0u, 1072094276u, 0u, 1072098943u, 0u, 1072103591u, 0u, 1072108222u, 0u, 1072112835u, 0u, 1072117430u, 0u, 1072122007u, 0u, 1072126567u, 0u, 1072131108u, 0u, 1072135633u, 0u, 1072140139u, 0u, 1072144628u, 0u, 1072149100u, 0u, 1072153555u, 0u, 1072157992u, 0u, 1072162412u, 0u, 1072166814u, 0u, 1072171200u, 0u, 1072175568u, 0u, 1072179920u, 0u, 1072184254u, 0u, 1072188572u, 0u, 1072192873u, 0u, 1072197157u, 0u, 1072201424u, 0u, 1072205675u, 0u, 1072209909u, 0u, 1072214127u, 0u, 1072218329u, 0u, 1072222514u, 0u, 1072226682u, 0u, 1072230835u, 0u, 1072234971u, 0u, 1072239091u, 0u, 1072243195u);
var<private> glob3: array<u32, 482> = array<u32, 482>(3174619948u, 1045882271u, 1423892374u, 1046799274u, 2870729352u, 1046995205u, 1291927504u, 1044956453u, 987113072u, 1043134635u, 3200779368u, 1046076580u, 3969427674u, 1045689525u, 1637712382u, 1047240009u, 2269514036u, 1044578680u, 3389757764u, 1045503550u, 1012544302u, 1045754871u, 2920153254u, 1046245832u, 4168072647u, 1046990304u, 2357763186u, 1045892914u, 2610515710u, 1046218670u, 494605976u, 1044432898u, 3988310818u, 1046697349u, 1739787926u, 1047561253u, 271664942u, 1048157825u, 337034478u, 1046495618u, 3839661812u, 1048535174u, 1549736792u, 1044847187u, 1072053409u, 1047945076u, 879939604u, 1048427891u, 1300958493u, 1048385740u, 1405680425u, 1048101157u, 3806919057u, 1047870281u, 3779641665u, 1048003114u, 1776244392u, 1045315266u, 1316883145u, 1048573557u, 2065390253u, 1047605542u, 1077580988u, 1048384762u, 3230245848u, 1046672278u, 2029688251u, 1048343325u, 2232906649u, 1048334579u, 2624531488u, 1047478592u, 3301154496u, 1048391056u, 1768890630u, 1047004192u, 2034475752u, 1045119026u, 49533602u, 1046860638u, 679548848u, 1046783128u, 430945060u, 1046699425u, 3451496712u, 1047613061u, 3847519130u, 1046710100u, 3872272966u, 1048306698u, 3015616487u, 1047579027u, 2309763929u, 1047646941u, 53685318u, 1046562662u, 464278967u, 1048520080u, 2088922664u, 1048545307u, 1835633952u, 1047806361u, 1377738889u, 1048840108u, 1816860451u, 1048771340u, 2523545112u, 1049403701u, 3839248100u, 1047204376u, 2739982984u, 1047350898u, 788841257u, 1048863102u, 1105733200u, 1046517484u, 628571758u, 1048181023u, 552719988u, 1046846472u, 2732527716u, 1046490103u, 4232073359u, 1049604971u, 1066256530u, 1048486031u, 1415932430u, 1047784802u, 3857095768u, 1047233525u, 2943780242u, 1049456322u, 2585948052u, 1046778760u, 2790422970u, 1048948111u, 2228507700u, 1047224663u, 1130950981u, 1048615448u, 967891228u, 1047927332u, 1580837240u, 1048371622u, 2839243366u, 1048783417u, 2599945447u, 1048691561u, 1313140200u, 1045913103u, 3517425723u, 1049589710u, 212972800u, 1042828927u, 951070206u, 1049544376u, 3753826951u, 1048833012u, 3986470005u, 1049180538u, 3156450388u, 1047558455u, 1429287350u, 1049274196u, 1081886172u, 1047830621u, 438332880u, 1048654371u, 4258700424u, 1047714028u, 1948354054u, 1048216876u, 3136207696u, 1049041906u, 3075020832u, 1044921725u, 2461456916u, 1047307533u, 3897754264u, 1045886413u, 2559641408u, 1048523825u, 1103943690u, 1047587026u, 3323586248u, 1047614298u, 1103326418u, 1047998816u, 6520206u, 1048151456u, 2240547432u, 1047473961u, 2140592393u, 1049131396u, 434187842u, 1049277395u, 1658832066u, 1047857281u, 2714606988u, 1046501193u, 1818986008u, 1046324828u, 2112990564u, 1046952383u, 1386885952u, 1047249682u, 2444049064u, 1046368897u, 1039245080u, 1049176537u, 3063128192u, 1046475038u, 4047872928u, 1044732643u, 3334523522u, 1048362200u, 3559248644u, 1047579396u, 2874926712u, 1048161788u, 3311805704u, 1049181584u, 3174432576u, 1047954569u, 1034074080u, 1043695341u, 780472046u, 1049059405u, 737891288u, 1048232401u, 1747404608u, 1043968980u, 2366694716u, 1048561110u, 411352779u, 1049164261u, 2258372305u, 1049266160u, 2046199290u, 1048780402u, 798431664u, 1044817960u, 1250575584u, 1046532722u, 1259449678u, 1049256018u, 2200391920u, 1045913567u, 48549174u, 1049423977u, 760179984u, 1047429698u, 274982592u, 1045580297u, 1927049016u, 1048093089u, 435726637u, 1049795430u, 2619044040u, 1048127309u, 532379952u, 1045480267u, 1814096720u, 1046886790u, 4135792286u, 1049024208u, 4019843678u, 1050342733u, 1386218082u, 1049718012u, 2203491378u, 1049480222u, 1428083749u, 1049853818u, 2915666055u, 1050633056u, 2369102529u, 1049806572u, 4165775414u, 1049351245u, 2188259598u, 1049696207u, 4080193481u, 1050453299u, 1352514606u, 1049686536u, 2479441374u, 1049415805u, 3598609778u, 1049983967u, 3306604120u, 1048226697u, 2980165792u, 1046734883u, 1251831100u, 1048774470u, 3443755075u, 1050433939u, 2687153492u, 1050387196u, 1770234472u, 1048582747u, 3008425928u, 1046639569u, 2352459974u, 1048988707u, 1936761600u, 1047191248u, 427734142u, 1049067665u, 1172363964u, 1047834866u, 2035166895u, 1049657561u, 1119226998u, 1049448130u, 4181772960u, 1044521426u, 724286044u, 1048370116u, 3141797952u, 1047318071u, 2580487632u, 1050067417u, 132843632u, 1046211978u, 2403929660u, 1047961304u, 431439600u, 1047319575u, 3378201015u, 1050459210u, 3096507863u, 1049770225u, 2183503044u, 1047858242u, 3733435362u, 1050088402u, 3525661302u, 1048696231u, 1210219201u, 1050414589u, 3669452627u, 1049741668u, 3580904598u, 1049026134u, 533829992u, 1048882871u, 3313178904u, 1049608081u, 2143182234u, 1050504175u, 3693383782u, 1049912531u, 3964506227u, 1050032354u, 1917315708u, 1047621450u, 1089590336u, 1043439783u, 18129724u, 1049030075u, 3206369596u, 1048428264u, 2955186035u, 1049999549u, 1063169124u, 1050140221u, 2276344970u, 1049551425u, 2448183698u, 1050543813u, 439559992u, 1047847814u, 3703750628u, 1048221081u, 3134469224u, 1047491375u, 1941383129u, 1050589685u, 2733349015u, 1050194450u, 1276539413u, 1049855112u, 1661075537u, 1049682643u, 2046545954u, 1049788936u, 2302561072u, 1050286777u, 396510328u, 1048760760u, 2381138040u, 1046568315u, 2645796324u, 1048090255u, 1912476511u, 1050090561u, 1616711052u, 1050073103u, 1130960272u, 1048351216u, 368894642u, 1048813450u, 2849590144u, 1044732909u, 1566610586u, 1049301697u, 3004350435u, 1049772391u, 856097399u, 1049668486u, 3954999534u, 1048921428u, 4015672248u, 1046487114u, 3329871660u, 1050193672u, 856352456u, 1049658666u, 2663590166u, 1049023467u, 2144058110u, 1049003467u, 3252731285u, 1049753425u, 2968986976u, 1045890481u, 3624454802u, 1050384600u, 3344746880u, 1046669858u, 734659408u, 1050127270u, 876750732u, 1050506115u, 531729401u, 1049994698u, 728682280u, 1046586889u, 1235927300u, 1047810000u, 3638514048u, 1043075407u, 319810277u, 1049990959u, 2774524312u, 1048354868u, 1234764327u, 1050075825u, 43276588u, 1048248320u, 3292684814u, 1050196413u, 4206837326u, 1049466109u, 2974133658u, 1048959792u, 3109036720u, 1049509406u, 1162095574u, 1050499979u, 589221166u, 1050124358u, 1056264754u, 1050667774u, 3218571879u, 1050163687u, 2641340672u, 1046764788u, 724038528u, 1045160723u, 196628383u, 1049965464u, 3460014992u, 1045989858u, 3282898120u, 1048541048u, 3469011874u, 1048864352u, 1175561766u, 1048908043u);
var<private> glob4: array<u32, 128> = array<u32, 128>(0u, 1072693248u, 805306368u, 1072704666u, 3489660928u, 1072716208u, 268435456u, 1072727877u, 1610612736u, 1072739672u, 805306368u, 1072751596u, 3489660928u, 1072763649u, 2684354560u, 1072775834u, 805306368u, 1072788152u, 3758096384u, 1072800603u, 805306368u, 1072813191u, 2147483648u, 1072825915u, 1610612736u, 1072838778u, 1610612736u, 1072851781u, 4026531840u, 1072864925u, 2684354560u, 1072878213u, 0u, 1072891646u, 2952790016u, 1072905224u, 805306368u, 1072918951u, 805306368u, 1072932827u, 1073741824u, 1072946854u, 536870912u, 1072961034u, 1610612736u, 1072975368u, 2952790016u, 1072989858u, 3489660928u, 1073004506u, 1879048192u, 1073019314u, 1342177280u, 1073034283u, 805306368u, 1073049415u, 3489660928u, 1073064711u, 268435456u, 1073080175u, 2952790016u, 1073095806u, 2147483648u, 1073111608u, 1610612736u, 1073127582u, 805306368u, 1073143730u, 3758096384u, 1073160053u, 1342177280u, 1073176555u, 1879048192u, 1073193236u, 805306368u, 1073210099u, 2415919104u, 1073227145u, 2415919104u, 1073244377u, 1073741824u, 1073261797u, 2415919104u, 1073279406u, 2952790016u, 1073297207u, 2415919104u, 1073315202u, 2147483648u, 1073333393u, 1879048192u, 1073351782u, 2952790016u, 1073370371u, 1342177280u, 1073389163u, 2415919104u, 1073408159u, 2952790016u, 1073427362u, 4026531840u, 1073446774u, 2415919104u, 1073466398u, 3489660928u, 1073486235u, 536870912u, 1073506289u, 3489660928u, 1073526560u, 1073741824u, 1073547053u, 3489660928u, 1073567768u, 0u, 1073588710u, 805306368u, 1073609879u, 3758096384u, 1073631278u, 2684354560u, 1073652911u, 3758096384u, 1073674779u, 1342177280u, 1073696886u, 2147483648u, 1073719233u);
var<private> glob5: array<u32, 128> = array<u32, 128>(0u, 0u, 3252477689u, 1047326464u, 2715237930u, 1044950059u, 2417067710u, 1046539063u, 516239730u, 1047130898u, 269498903u, 1044815501u, 2766913306u, 1039293264u, 3211166211u, 1047351899u, 4122590969u, 1047067298u, 3933826353u, 1046929302u, 2810207104u, 1043762074u, 2893487574u, 1046529305u, 1892516853u, 1047325380u, 1955981724u, 1045924339u, 2226165573u, 1045725051u, 754984053u, 1046188044u, 710660523u, 1046766446u, 4200916016u, 1044586679u, 714887508u, 1046276775u, 3685248804u, 1045665407u, 1147889700u, 1047012456u, 2676240987u, 1044345570u, 1086643152u, 1041785419u, 1078920846u, 1045890291u, 2624917086u, 1045747880u, 2595540834u, 1046115506u, 125577743u, 1046115646u, 1771811520u, 1046140063u, 1387369056u, 1047171240u, 565866387u, 1045869701u, 1492679939u, 1041050306u, 612974287u, 1044556049u, 843197630u, 1046084847u, 1575240133u, 1047054900u, 3887744737u, 1046599870u, 587053674u, 1046022559u, 918464641u, 1045387276u, 2285990922u, 1046166841u, 622173969u, 1046649244u, 3640772667u, 1046907173u, 3671932458u, 1044468998u, 1817653326u, 1046620910u, 2659844999u, 1042921660u, 2669100324u, 1047510172u, 2162418229u, 1044717444u, 3378853301u, 1046920138u, 3831108132u, 1044508970u, 4245662864u, 1045817334u, 1522339433u, 1046656423u, 4125982625u, 1046519339u, 928333188u, 1044896498u, 2300504125u, 1041428596u, 943997334u, 1047202469u, 2526298735u, 1047310242u, 3525969815u, 1047125792u, 4145684037u, 1046744850u, 239842315u, 1047131977u, 730095629u, 1045354900u, 4123739734u, 1045159130u, 3634164812u, 1046359295u, 3425467293u, 1044718726u, 1324236708u, 1047315124u, 2161024777u, 1046928522u, 2196310654u, 1043982605u);
var<private> glob6: array<u32, 40> = array<u32, 40>(3239834080u, 1948342284u, 2126806581u, 1269411821u, 3890096458u, 3320640540u, 3944722327u, 2830488734u, 4254759295u, 652033304u, 4215054124u, 3028137177u, 1177380905u, 1614789820u, 1610577942u, 2699853794u, 2117007332u, 3889328657u, 1491471692u, 4177847879u, 3231371645u, 303277681u, 3621042925u, 2633736712u, 2893202598u, 1215559799u, 3148359494u, 1266686244u, 3102279991u, 361159825u, 2393864943u, 1091041605u, 1449455886u, 1996713354u, 1598543059u, 1420292253u, 230242576u, 166886u, 0u, 0u);
fn ld_args(p: u32) -> u32 {
    let i = (p - 4096u) >> 2u;
    let s = (p & 3u) * 8u;
    if (s == 0u) { return args[i]; }
    return (args[i] >> s) | (args[i + 1u] << (32u - s));
}
fn ld_buf0(p: u32) -> u32 {
    let i = (p - 4096u) >> 2u;
    let s = (p & 3u) * 8u;
    if (s == 0u) { return buf0[i]; }
    return (buf0[i] >> s) | (buf0[i + 1u] << (32u - s));
}
fn st_buf0(p: u32, v: u32, bytes: u32) {
    if ((p & 3u) == 0u && bytes == 4u) { buf0[(p - 4096u) >> 2u] = v; return; }
    for (var k = 0u; k < bytes; k++) {
        let q = p + k;
        let i = (q - 4096u) >> 2u;
        let s = (q & 3u) * 8u;
        buf0[i] = (buf0[i] & ~(0xffu << s)) | (((v >> (8u * k)) & 0xffu) << s);
    }
}
fn ld_buf1(p: u32) -> u32 {
    let i = (p - 4096u) >> 2u;
    let s = (p & 3u) * 8u;
    if (s == 0u) { return buf1[i]; }
    return (buf1[i] >> s) | (buf1[i + 1u] << (32u - s));
}
fn st_buf1(p: u32, v: u32, bytes: u32) {
    if ((p & 3u) == 0u && bytes == 4u) { buf1[(p - 4096u) >> 2u] = v; return; }
    for (var k = 0u; k < bytes; k++) {
        let q = p + k;
        let i = (q - 4096u) >> 2u;
        let s = (q & 3u) * 8u;
        buf1[i] = (buf1[i] & ~(0xffu << s)) | (((v >> (8u * k)) & 0xffu) << s);
    }
}
fn ld_buf2(p: u32) -> u32 {
    let i = (p - 4096u) >> 2u;
    let s = (p & 3u) * 8u;
    if (s == 0u) { return buf2[i]; }
    return (buf2[i] >> s) | (buf2[i + 1u] << (32u - s));
}
fn st_buf2(p: u32, v: u32, bytes: u32) {
    if ((p & 3u) == 0u && bytes == 4u) { buf2[(p - 4096u) >> 2u] = v; return; }
    for (var k = 0u; k < bytes; k++) {
        let q = p + k;
        let i = (q - 4096u) >> 2u;
        let s = (q & 3u) * 8u;
        buf2[i] = (buf2[i] & ~(0xffu << s)) | (((v >> (8u * k)) & 0xffu) << s);
    }
}
fn ld_glob0(p: u32) -> u32 {
    let i = (p - 4096u) >> 2u;
    let s = (p & 3u) * 8u;
    if (s == 0u) { return glob0[i]; }
    return (glob0[i] >> s) | (glob0[i + 1u] << (32u - s));
}
fn st_glob0(p: u32, v: u32, bytes: u32) {
    if ((p & 3u) == 0u && bytes == 4u) { glob0[(p - 4096u) >> 2u] = v; return; }
    for (var k = 0u; k < bytes; k++) {
        let q = p + k;
        let i = (q - 4096u) >> 2u;
        let s = (q & 3u) * 8u;
        glob0[i] = (glob0[i] & ~(0xffu << s)) | (((v >> (8u * k)) & 0xffu) << s);
    }
}
fn ld_glob1(p: u32) -> u32 {
    let i = (p - 4096u) >> 2u;
    let s = (p & 3u) * 8u;
    if (s == 0u) { return glob1[i]; }
    return (glob1[i] >> s) | (glob1[i + 1u] << (32u - s));
}
fn st_glob1(p: u32, v: u32, bytes: u32) {
    if ((p & 3u) == 0u && bytes == 4u) { glob1[(p - 4096u) >> 2u] = v; return; }
    for (var k = 0u; k < bytes; k++) {
        let q = p + k;
        let i = (q - 4096u) >> 2u;
        let s = (q & 3u) * 8u;
        glob1[i] = (glob1[i] & ~(0xffu << s)) | (((v >> (8u * k)) & 0xffu) << s);
    }
}
fn ld_glob2(p: u32) -> u32 {
    let i = (p - 4096u) >> 2u;
    let s = (p & 3u) * 8u;
    if (s == 0u) { return glob2[i]; }
    return (glob2[i] >> s) | (glob2[i + 1u] << (32u - s));
}
fn st_glob2(p: u32, v: u32, bytes: u32) {
    if ((p & 3u) == 0u && bytes == 4u) { glob2[(p - 4096u) >> 2u] = v; return; }
    for (var k = 0u; k < bytes; k++) {
        let q = p + k;
        let i = (q - 4096u) >> 2u;
        let s = (q & 3u) * 8u;
        glob2[i] = (glob2[i] & ~(0xffu << s)) | (((v >> (8u * k)) & 0xffu) << s);
    }
}
fn ld_glob3(p: u32) -> u32 {
    let i = (p - 4096u) >> 2u;
    let s = (p & 3u) * 8u;
    if (s == 0u) { return glob3[i]; }
    return (glob3[i] >> s) | (glob3[i + 1u] << (32u - s));
}
fn st_glob3(p: u32, v: u32, bytes: u32) {
    if ((p & 3u) == 0u && bytes == 4u) { glob3[(p - 4096u) >> 2u] = v; return; }
    for (var k = 0u; k < bytes; k++) {
        let q = p + k;
        let i = (q - 4096u) >> 2u;
        let s = (q & 3u) * 8u;
        glob3[i] = (glob3[i] & ~(0xffu << s)) | (((v >> (8u * k)) & 0xffu) << s);
    }
}
fn ld_glob4(p: u32) -> u32 {
    let i = (p - 4096u) >> 2u;
    let s = (p & 3u) * 8u;
    if (s == 0u) { return glob4[i]; }
    return (glob4[i] >> s) | (glob4[i + 1u] << (32u - s));
}
fn st_glob4(p: u32, v: u32, bytes: u32) {
    if ((p & 3u) == 0u && bytes == 4u) { glob4[(p - 4096u) >> 2u] = v; return; }
    for (var k = 0u; k < bytes; k++) {
        let q = p + k;
        let i = (q - 4096u) >> 2u;
        let s = (q & 3u) * 8u;
        glob4[i] = (glob4[i] & ~(0xffu << s)) | (((v >> (8u * k)) & 0xffu) << s);
    }
}
fn ld_glob5(p: u32) -> u32 {
    let i = (p - 4096u) >> 2u;
    let s = (p & 3u) * 8u;
    if (s == 0u) { return glob5[i]; }
    return (glob5[i] >> s) | (glob5[i + 1u] << (32u - s));
}
fn st_glob5(p: u32, v: u32, bytes: u32) {
    if ((p & 3u) == 0u && bytes == 4u) { glob5[(p - 4096u) >> 2u] = v; return; }
    for (var k = 0u; k < bytes; k++) {
        let q = p + k;
        let i = (q - 4096u) >> 2u;
        let s = (q & 3u) * 8u;
        glob5[i] = (glob5[i] & ~(0xffu << s)) | (((v >> (8u * k)) & 0xffu) << s);
    }
}
fn ld_glob6(p: u32) -> u32 {
    let i = (p - 4096u) >> 2u;
    let s = (p & 3u) * 8u;
    if (s == 0u) { return glob6[i]; }
    return (glob6[i] >> s) | (glob6[i + 1u] << (32u - s));
}
fn st_glob6(p: u32, v: u32, bytes: u32) {
    if ((p & 3u) == 0u && bytes == 4u) { glob6[(p - 4096u) >> 2u] = v; return; }
    for (var k = 0u; k < bytes; k++) {
        let q = p + k;
        let i = (q - 4096u) >> 2u;
        let s = (q & 3u) * 8u;
        glob6[i] = (glob6[i] & ~(0xffu << s)) | (((v >> (8u * k)) & 0xffu) << s);
    }
}
fn mul_hi_u32(a: u32, b: u32) -> u32 {
    let al = a & 0xffffu;
    let ah = a >> 16u;
    let bl = b & 0xffffu;
    let bh = b >> 16u;
    let lh = al * bh;
    let hl = ah * bl;
    let mid = ((al * bl) >> 16u) + (lh & 0xffffu) + (hl & 0xffffu);
    return ah * bh + (lh >> 16u) + (hl >> 16u) + (mid >> 16u);
}
fn add64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let lo = a.x + b.x;
    return vec2<u32>(lo, a.y + b.y + select(0u, 1u, lo < a.x));
}
fn sub64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    return vec2<u32>(a.x - b.x, a.y - b.y - select(0u, 1u, a.x < b.x));
}
fn neg64(a: vec2<u32>) -> vec2<u32> { return sub64(vec2<u32>(0u, 0u), a); }
fn mul64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    return vec2<u32>(a.x * b.x, mul_hi_u32(a.x, b.x) + a.x * b.y + a.y * b.x);
}
fn shl64(a: vec2<u32>, s: u32) -> vec2<u32> {
    let n = s & 63u;
    if (n == 0u) { return a; }
    if (n >= 32u) { return vec2<u32>(0u, a.x << (n - 32u)); }
    return vec2<u32>(a.x << n, (a.y << n) | (a.x >> (32u - n)));
}
fn lshr64(a: vec2<u32>, s: u32) -> vec2<u32> {
    let n = s & 63u;
    if (n == 0u) { return a; }
    if (n >= 32u) { return vec2<u32>(a.y >> (n - 32u), 0u); }
    return vec2<u32>((a.x >> n) | (a.y << (32u - n)), a.y >> n);
}
fn ashr64(a: vec2<u32>, s: u32) -> vec2<u32> {
    let n = s & 63u;
    let h = bitcast<i32>(a.y);
    if (n == 0u) { return a; }
    if (n >= 32u) { return vec2<u32>(bitcast<u32>(h >> (n - 32u)), bitcast<u32>(h >> 31u)); }
    return vec2<u32>((a.x >> n) | (a.y << (32u - n)), bitcast<u32>(h >> n));
}
fn ult64(a: vec2<u32>, b: vec2<u32>) -> bool { return a.y < b.y || (a.y == b.y && a.x < b.x); }
fn slt64(a: vec2<u32>, b: vec2<u32>) -> bool {
    let ah = bitcast<i32>(a.y);
    let bh = bitcast<i32>(b.y);
    return ah < bh || (ah == bh && a.x < b.x);
}
fn umulhi64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let p01 = vec2<u32>(a.x * b.y, mul_hi_u32(a.x, b.y));
    let p10 = vec2<u32>(a.y * b.x, mul_hi_u32(a.y, b.x));
    let p11 = vec2<u32>(a.y * b.y, mul_hi_u32(a.y, b.y));
    let m1 = add64(p01, vec2<u32>(mul_hi_u32(a.x, b.x), 0u));
    let m2 = add64(m1, p10);
    let carry = select(0u, 1u, ult64(m2, m1));
    return add64(p11, vec2<u32>(m2.y, carry));
}
fn u128_add(a: vec4<u32>, b: vec4<u32>) -> vec4<u32> {
    let x = a.x + b.x;
    let c0 = select(0u, 1u, x < a.x);
    let y1 = a.y + b.y;
    let y = y1 + c0;
    let c1 = select(0u, 1u, y1 < a.y || y < y1);
    let z1 = a.z + b.z;
    let z = z1 + c1;
    let c2 = select(0u, 1u, z1 < a.z || z < z1);
    return vec4<u32>(x, y, z, a.w + b.w + c2);
}
fn u128_sub(a: vec4<u32>, b: vec4<u32>) -> vec4<u32> {
    let b0 = select(0u, 1u, a.x < b.x);
    let y1 = a.y - b.y;
    let b1 = select(0u, 1u, a.y < b.y || y1 < b0);
    let z1 = a.z - b.z;
    let b2 = select(0u, 1u, a.z < b.z || z1 < b1);
    return vec4<u32>(a.x - b.x, y1 - b0, z1 - b1, a.w - b.w - b2);
}
fn u128_lt(a: vec4<u32>, b: vec4<u32>) -> bool {
    if (a.w != b.w) { return a.w < b.w; }
    if (a.z != b.z) { return a.z < b.z; }
    if (a.y != b.y) { return a.y < b.y; }
    return a.x < b.x;
}
fn u128_clz(a: vec4<u32>) -> u32 {
    if (a.w != 0u) { return countLeadingZeros(a.w); }
    if (a.z != 0u) { return 32u + countLeadingZeros(a.z); }
    if (a.y != 0u) { return 64u + countLeadingZeros(a.y); }
    return 96u + countLeadingZeros(a.x);
}
fn u128_shl(a: vec4<u32>, n: u32) -> vec4<u32> {
    var r = a;
    var k = n;
    if (k >= 64u) { r = vec4<u32>(0u, 0u, r.x, r.y); k -= 64u; }
    if (k >= 32u) { r = vec4<u32>(0u, r.x, r.y, r.z); k -= 32u; }
    if (k == 0u) { return r; }
    return vec4<u32>(r.x << k, (r.y << k) | (r.x >> (32u - k)), (r.z << k) | (r.y >> (32u - k)), (r.w << k) | (r.z >> (32u - k)));
}
fn u128_shr_sticky(a: vec4<u32>, n: u32) -> vec4<u32> {
    if (n == 0u) { return a; }
    if (n >= 128u) { return vec4<u32>(select(0u, 1u, any(a != vec4<u32>(0u))), 0u, 0u, 0u); }
    var r = a;
    var k = n;
    var lost = 0u;
    if (k >= 64u) { lost = r.x | r.y; r = vec4<u32>(r.z, r.w, 0u, 0u); k -= 64u; }
    if (k >= 32u) { lost = lost | r.x; r = vec4<u32>(r.y, r.z, r.w, 0u); k -= 32u; }
    if (k > 0u) {
        lost = lost | (r.x << (32u - k));
        r = vec4<u32>((r.x >> k) | (r.y << (32u - k)), (r.y >> k) | (r.z << (32u - k)), (r.z >> k) | (r.w << (32u - k)), r.w >> k);
    }
    r.x = r.x | select(0u, 1u, lost != 0u);
    return r;
}
fn f64_is_nan(a: vec2<u32>) -> bool {
    let h = a.y & 0x7fffffffu;
    return h > 0x7ff00000u || (h == 0x7ff00000u && a.x != 0u);
}
fn f64_is_inf(a: vec2<u32>) -> bool { return (a.y & 0x7fffffffu) == 0x7ff00000u && a.x == 0u; }
fn f64_is_zero(a: vec2<u32>) -> bool { return (a.y & 0x7fffffffu) == 0u && a.x == 0u; }
fn f64_quiet(a: vec2<u32>) -> vec2<u32> { return vec2<u32>(a.x, a.y | 0x00080000u); }
fn f64_mant(a: vec2<u32>) -> vec2<u32> {
    let e = (a.y >> 20u) & 0x7ffu;
    return vec2<u32>(a.x, (a.y & 0xfffffu) | select(0u, 0x100000u, e != 0u));
}
fn f64_exp(a: vec2<u32>) -> i32 { return i32(max((a.y >> 20u) & 0x7ffu, 1u)) - 1075i; }
fn f64_round(sign: u32, m: vec4<u32>, e: i32) -> vec2<u32> {
    let top = 127i - i32(u128_clz(m));
    if (top + e + 1023i >= 2047i) { return vec2<u32>(0u, sign | 0x7ff00000u); }
    let d = max(top - 52i, -1074i - e);
    var q: vec2<u32>;
    if (d <= 0i) {
        q = u128_shl(m, u32(-d)).xy;
    } else {
        // (Its last two bits: the first bit dropped, and whether any other was.)
        var s: vec4<u32>;
        if (d >= 2i) { s = u128_shr_sticky(m, u32(d - 2i)); } else { s = u128_shl(m, 1u); }
        q = vec2<u32>((s.x >> 2u) | (s.y << 30u), (s.y >> 2u) | (s.z << 30u));
        let half = (s.x & 2u) != 0u;
        let more = (s.x & 1u) != 0u;
        if (half && (more || (q.x & 1u) != 0u)) { q = add64(q, vec2<u32>(1u, 0u)); }
    }
    let r = add64(vec2<u32>(0u, u32(e + d + 1074i) << 20u), q);
    return vec2<u32>(r.x, r.y | sign);
}
fn f64_fma(a: vec2<u32>, b: vec2<u32>, c: vec2<u32>) -> vec2<u32> {
    let nan = vec2<u32>(0u, 0x7ff80000u);
    if (f64_is_nan(a)) { return f64_quiet(a); }
    if (f64_is_nan(b)) { return f64_quiet(b); }
    let sp = (a.y ^ b.y) & 0x80000000u;
    let pz = f64_is_zero(a) || f64_is_zero(b);
    let pinf = f64_is_inf(a) || f64_is_inf(b);
    if (pinf && pz) { return nan; }
    if (f64_is_nan(c)) { return f64_quiet(c); }
    let sc = c.y & 0x80000000u;
    if (pinf) {
        if (f64_is_inf(c) && sc != sp) { return nan; }
        return vec2<u32>(0u, sp | 0x7ff00000u);
    }
    if (f64_is_inf(c)) { return c; }
    if (pz) {
        // (Zeros: +0 unless both are -0.)
        if (f64_is_zero(c)) { return vec2<u32>(0u, sp & sc); }
        return c;
    }
    let ma = f64_mant(a);
    let mb = f64_mant(b);
    let p = vec4<u32>(mul64(ma, mb), umulhi64(ma, mb));
    let ep = f64_exp(a) + f64_exp(b);
    if (f64_is_zero(c)) { return f64_round(sp, p, ep); }
    let cm = vec4<u32>(f64_mant(c), 0u, 0u);
    let ec = f64_exp(c);
    // Summed in a frame where the one with the higher leading bit has it
    // at bit 125; the other's bits below the frame are sticky.
    var x = p;
    var ex = ep;
    var sx = sp;
    var y = cm;
    var ey = ec;
    var sy = sc;
    if (127i - i32(u128_clz(cm)) + ec > 127i - i32(u128_clz(p)) + ep) {
        x = cm; ex = ec; sx = sc;
        y = p; ey = ep; sy = sp;
    }
    let l = 125i - (127i - i32(u128_clz(x)));
    x = u128_shl(x, u32(l));
    let e0 = ex - l;
    if (ey < e0) { y = u128_shr_sticky(y, u32(e0 - ey)); } else if (ey > e0) { y = u128_shl(y, u32(ey - e0)); }
    var m: vec4<u32>;
    var s = sx;
    if (sx == sy) {
        m = u128_add(x, y);
    } else if (u128_lt(x, y)) {
        m = u128_sub(y, x);
        s = sy;
    } else {
        m = u128_sub(x, y);
    }
    if (all(m == vec4<u32>(0u))) { return vec2<u32>(0u, 0u); }
    return f64_round(s, m, e0);
}
fn u64_shr_sticky(a: vec2<u32>, n: u32) -> vec2<u32> {
    if (n == 0u) { return a; }
    if (n >= 64u) { return vec2<u32>(select(0u, 1u, (a.x | a.y) != 0u), 0u); }
    let r = lshr64(a, n);
    let lost = a & ~shl64(vec2<u32>(0xffffffffu, 0xffffffffu), n);
    return vec2<u32>(r.x | select(0u, 1u, (lost.x | lost.y) != 0u), r.y);
}
fn f64_add(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    if (f64_is_nan(a)) { return f64_quiet(a); }
    if (f64_is_nan(b)) { return f64_quiet(b); }
    var sa = a.y & 0x80000000u;
    var sb = b.y & 0x80000000u;
    if (f64_is_inf(a)) {
        if (f64_is_inf(b) && sa != sb) { return vec2<u32>(0u, 0x7ff80000u); }
        return a;
    }
    if (f64_is_inf(b)) { return b; }
    if (f64_is_zero(a)) {
        if (f64_is_zero(b)) { return vec2<u32>(0u, sa & sb); }
        return b;
    }
    if (f64_is_zero(b)) { return a; }
    var ma = shl64(f64_mant(a), 10u);
    var ea = f64_exp(a) - 10i;
    var mb = shl64(f64_mant(b), 10u);
    var eb = f64_exp(b) - 10i;
    if (eb > ea) {
        let m = ma; ma = mb; mb = m;
        let e = ea; ea = eb; eb = e;
        let s = sa; sa = sb; sb = s;
    }
    mb = u64_shr_sticky(mb, u32(ea - eb));
    var m: vec2<u32>;
    var s = sa;
    if (sa == sb) {
        m = add64(ma, mb);
    } else if (ult64(ma, mb)) {
        m = sub64(mb, ma);
        s = sb;
    } else {
        m = sub64(ma, mb);
    }
    if ((m.x | m.y) == 0u) { return vec2<u32>(0u, 0u); }
    return f64_round(s, vec4<u32>(m, 0u, 0u), ea);
}
fn f64_sub(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> { return f64_add(a, vec2<u32>(b.x, b.y ^ 0x80000000u)); }
fn f64_mul(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    if (f64_is_nan(a)) { return f64_quiet(a); }
    if (f64_is_nan(b)) { return f64_quiet(b); }
    let s = (a.y ^ b.y) & 0x80000000u;
    let z = f64_is_zero(a) || f64_is_zero(b);
    let i = f64_is_inf(a) || f64_is_inf(b);
    if (z && i) { return vec2<u32>(0u, 0x7ff80000u); }
    if (i) { return vec2<u32>(0u, s | 0x7ff00000u); }
    if (z) { return vec2<u32>(0u, s); }
    let ma = f64_mant(a);
    let mb = f64_mant(b);
    return f64_round(s, vec4<u32>(mul64(ma, mb), umulhi64(ma, mb)), f64_exp(a) + f64_exp(b));
}
fn f64_normal(a: vec2<u32>) -> vec3<u32> {
    let m = f64_mant(a);
    let z = select(32u + countLeadingZeros(m.x), countLeadingZeros(m.y), m.y != 0u) - 11u;
    return vec3<u32>(shl64(m, z), bitcast<u32>(f64_exp(a) - i32(z)));
}
fn f64_div(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let nan = vec2<u32>(0u, 0x7ff80000u);
    if (f64_is_nan(a)) { return f64_quiet(a); }
    if (f64_is_nan(b)) { return f64_quiet(b); }
    let s = (a.y ^ b.y) & 0x80000000u;
    if (f64_is_inf(a)) {
        if (f64_is_inf(b)) { return nan; }
        return vec2<u32>(0u, s | 0x7ff00000u);
    }
    if (f64_is_inf(b)) { return vec2<u32>(0u, s); }
    if (f64_is_zero(b)) {
        if (f64_is_zero(a)) { return nan; }
        return vec2<u32>(0u, s | 0x7ff00000u);
    }
    if (f64_is_zero(a)) { return vec2<u32>(0u, s); }
    let na = f64_normal(a);
    let nb = f64_normal(b);
    let d = nb.xy;
    var r = na.xy;
    var q = vec2<u32>(0u, 0u);
    for (var i = 0u; i < 64u; i++) {
        q = shl64(q, 1u);
        if (!ult64(r, d)) { r = sub64(r, d); q.x = q.x | 1u; }
        r = shl64(r, 1u);
    }
    let m = vec4<u32>(q.x | select(0u, 1u, (r.x | r.y) != 0u), q.y, 0u, 0u);
    return f64_round(s, m, bitcast<i32>(na.z) - bitcast<i32>(nb.z) - 63i);
}
fn f64_sqrt(a: vec2<u32>) -> vec2<u32> {
    if (f64_is_nan(a)) { return f64_quiet(a); }
    if (f64_is_zero(a)) { return a; }
    if ((a.y & 0x80000000u) != 0u) { return vec2<u32>(0u, 0x7ff80000u); }
    if (a.y >= 0x7ff00000u) { return a; }
    let n = f64_normal(a);
    var m = n.xy;
    var e = bitcast<i32>(n.z);
    if ((e & 1i) != 0i) { m = shl64(m, 1u); e -= 1i; }
    let rad = u128_shl(vec4<u32>(m, 0u, 0u), 66u);
    var rem = vec2<u32>(0u, 0u);
    var s = vec2<u32>(0u, 0u);
    for (var i = 0u; i < 64u; i++) {
        let p = 126u - 2u * i;
        rem = shl64(rem, 2u);
        rem.x = rem.x | ((rad[p >> 5u] >> (p & 31u)) & 3u);
        let t = shl64(s, 2u) | vec2<u32>(1u, 0u);
        s = shl64(s, 1u);
        if (!ult64(rem, t)) { rem = sub64(rem, t); s.x = s.x | 1u; }
    }
    let r = vec4<u32>(s.x | select(0u, 1u, (rem.x | rem.y) != 0u), s.y, 0u, 0u);
    return f64_round(0u, r, (e - 66i) / 2i);
}
fn f64_to_f32(a: vec2<u32>) -> f32 {
    let s = a.y & 0x80000000u;
    if (f64_is_nan(a)) { return bitcast<f32>(s | 0x7fc00000u | ((a.y & 0x7ffffu) << 3u)); }
    if ((a.y & 0x7fffffffu) >= 0x7ff00000u) { return bitcast<f32>(s | 0x7f800000u); }
    if (f64_is_zero(a)) { return bitcast<f32>(s); }
    let m = f64_mant(a);
    let e = f64_exp(a);
    let top = select(31i - i32(countLeadingZeros(m.x)), 63i - i32(countLeadingZeros(m.y)), m.y != 0u);
    if (top + e + 127i >= 255i) { return bitcast<f32>(s | 0x7f800000u); }
    let d = max(top - 23i, -149i - e);
    let r = u128_shr_sticky(vec4<u32>(m, 0u, 0u), u32(d - 2i));
    var q = (r.x >> 2u) | (r.y << 30u);
    if ((r.x & 2u) != 0u && ((r.x & 1u) != 0u || (q & 1u) != 0u)) { q += 1u; }
    return bitcast<f32>(s | ((u32(e + d + 149i) << 23u) + q));
}
fn f64_from_u64(x: vec2<u32>) -> vec2<u32> {
    if ((x.x | x.y) == 0u) { return vec2<u32>(0u, 0u); }
    return f64_round(0u, vec4<u32>(x, 0u, 0u), 0i);
}
fn f64_from_i64(x: vec2<u32>) -> vec2<u32> {
    if ((x.x | x.y) == 0u) { return vec2<u32>(0u, 0u); }
    let s = x.y & 0x80000000u;
    return f64_round(s, vec4<u32>(select(x, neg64(x), s != 0u), 0u, 0u), 0i);
}
fn f64_to_u64(a: vec2<u32>) -> vec2<u32> {
    if (f64_is_nan(a) || (a.y & 0x80000000u) != 0u) { return vec2<u32>(0u, 0u); }
    let m = f64_mant(a);
    let e = f64_exp(a);
    if (a.y >= 0x7ff00000u || e > 11i) { return vec2<u32>(0xffffffffu, 0xffffffffu); }
    if (e >= 0i) { return shl64(m, u32(e)); }
    if (e <= -64i) { return vec2<u32>(0u, 0u); }
    return lshr64(m, u32(-e));
}
fn f64_to_i64(a: vec2<u32>) -> vec2<u32> {
    if (f64_is_nan(a)) { return vec2<u32>(0u, 0u); }
    let s = (a.y & 0x80000000u) != 0u;
    let m = f64_to_u64(vec2<u32>(a.x, a.y & 0x7fffffffu));
    if (s) {
        if (m.y >= 0x80000000u) { return vec2<u32>(0u, 0x80000000u); }
        return neg64(m);
    }
    if (m.y >= 0x80000000u) { return vec2<u32>(0xffffffffu, 0x7fffffffu); }
    return m;
}
fn f64_lt(a: vec2<u32>, b: vec2<u32>) -> bool {
    if (f64_is_nan(a) || f64_is_nan(b) || (f64_is_zero(a) && f64_is_zero(b))) { return false; }
    let ka = select(vec2<u32>(a.x, a.y | 0x80000000u), ~a, (a.y >> 31u) != 0u);
    let kb = select(vec2<u32>(b.x, b.y | 0x80000000u), ~b, (b.y >> 31u) != 0u);
    return ult64(ka, kb);
}
fn f64_eq(a: vec2<u32>, b: vec2<u32>) -> bool {
    return !f64_is_nan(a) && !f64_is_nan(b) && (all(a == b) || (f64_is_zero(a) && f64_is_zero(b)));
}
fn f64_trunc(a: vec2<u32>) -> vec2<u32> {
    let e = i32((a.y >> 20u) & 0x7ffu) - 1023i;
    if (e < 0i) { return vec2<u32>(0u, a.y & 0x80000000u); }
    if (e >= 52i) { return a; }
    return a & shl64(vec2<u32>(0xffffffffu, 0xffffffffu), u32(52i - e));
}
fn f64_floor(a: vec2<u32>) -> vec2<u32> {
    let t = f64_trunc(a);
    if ((a.y >> 31u) != 0u && !all(t == a)) { return f64_sub(t, vec2<u32>(0u, 0x3ff00000u)); }
    return t;
}
fn f64_min(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    if (f64_is_nan(a)) { return b; }
    if (f64_is_nan(b)) { return a; }
    return select(a, b, f64_lt(b, a));
}

@compute @workgroup_size(4, 1, 1)
fn main(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>) {
    let grp = wid + vec3<u32>(args[7], args[8], args[9]);
    let gid = grp * vec3<u32>(4u, 1u, 1u) + lid + vec3<u32>(args[0], args[1], args[2]);
    var jump: u32 = 0u;
    let a0 = select(args[16] + 4096u, 0u, args[16] == 0xffffffffu);
    let a1 = select(args[17] + 4096u, 0u, args[17] == 0xffffffffu);
    let a2 = select(args[18] + 4096u, 0u, args[18] == 0xffffffffu);
    var v0: u32;
    var v1: bool;
    var v2: u32;
    var v3: u32;
    var v4: u32;
    var v5: u32;
    var v6: u32;
    var v7: array<vec2<u32>, 2>;
    var v8: vec2<u32>;
    var v9: array<vec2<u32>, 2>;
    var v10: vec2<u32>;
    var v11: bool;
    var v12: vec2<u32>;
    var v13: vec2<u32>;
    var v14: vec2<u32>;
    var v15: u32;
    var v16: bool;
    var v17: vec2<u32>;
    var v18: vec2<u32>;
    var v19: vec2<u32>;
    var v20: u32;
    var v21: bool;
    var v22: bool;
    var v23: bool;
    var v24: bool;
    var v25: bool;
    var v26: vec2<u32>;
    var v27: vec2<u32>;
    var v28: vec2<u32>;
    var v29: vec2<u32>;
    var v30: vec2<u32>;
    var v31: vec2<u32>;
    var v32: vec2<u32>;
    var v33: u32;
    var v34: u32;
    var v35: bool;
    var v36: vec2<u32>;
    var v37: bool;
    var v38: vec2<u32>;
    var v39: vec2<u32>;
    var v40: vec2<u32>;
    var v41: vec2<u32>;
    var v42: vec2<u32>;
    var v43: vec2<u32>;
    var v44: vec2<bool>;
    var v45: bool;
    var v46: bool;
    var v47: bool;
    var v48: vec2<u32>;
    var v49: bool;
    var v50: bool;
    var v51: bool;
    var v52: vec2<u32>;
    var v53: vec2<u32>;
    var v54: vec2<u32>;
    var v55: vec2<u32>;
    var v56: vec2<u32>;
    var v57: vec2<u32>;
    var v58: vec2<u32>;
    var v59: vec2<u32>;
    var v60: vec2<u32>;
    var v61: vec2<u32>;
    var v62: vec2<u32>;
    var v63: vec2<u32>;
    var v64: vec2<u32>;
    var v65: vec2<u32>;
    var v66: vec2<u32>;
    var v67: vec2<u32>;
    var v68: vec2<u32>;
    var v69: vec2<u32>;
    var v70: vec2<u32>;
    var v71: vec2<u32>;
    var v72: vec2<u32>;
    var v73: vec2<u32>;
    var v74: vec2<u32>;
    var v75: vec2<u32>;
    var v76: vec2<u32>;
    var v77: vec2<u32>;
    var v78: bool;
    var v79: vec2<u32>;
    var v80: vec2<u32>;
    var v81: u32;
    var v82: u32;
    var v83: u32;
    var v84: u32;
    var v85: u32;
    var v86: vec2<u32>;
    var v87: vec2<u32>;
    var v88: vec2<u32>;
    var v89: u32;
    var v90: u32;
    var v91: u32;
    var v92: u32;
    var v93: u32;
    var v94: u32;
    var v95: u32;
    var v96: u32;
    var v97: vec2<u32>;
    var v98: u32;
    var v99: vec2<u32>;
    var v100: vec2<u32>;
    var v101: vec2<u32>;
    var v102: vec2<u32>;
    var v103: vec2<u32>;
    var v104: vec2<u32>;
    var v105: vec2<u32>;
    var v106: vec2<u32>;
    var v107: vec2<u32>;
    var v108: vec2<u32>;
    var v109: vec2<u32>;
    var v110: vec2<u32>;
    var v111: vec2<u32>;
    var v112: vec2<u32>;
    var v113: vec2<u32>;
    var v114: vec2<u32>;
    var v115: vec2<u32>;
    var v116: vec2<u32>;
    var v117: vec2<u32>;
    var v118: vec2<u32>;
    var v119: bool;
    var v120: bool;
    var v121: bool;
    var v122: vec2<u32>;
    var v123: vec2<u32>;
    var v124: vec2<u32>;
    var v125: vec2<u32>;
    var v126: bool;
    var v127: array<vec2<u32>, 2>;
    var v128: array<vec2<u32>, 2>;
    var v129: array<vec2<u32>, 2>;
    var v130: vec2<bool>;
    var v131: array<vec2<u32>, 2>;
    var v132: vec2<u32>;
    var v133: vec2<u32>;
    var v134: u32;
    var v135: bool;
    var v136: u32;
    var v137: bool;
    var v138: bool;
    var v139: array<vec2<u32>, 2>;
    var v140: array<vec2<u32>, 2>;
    var v141: vec2<u32>;
    var v142: vec2<u32>;
    var v143: vec2<u32>;
    var v144: vec2<bool>;
    var v145: vec2<u32>;
    var v146: vec2<u32>;
    var v147: array<vec2<u32>, 2>;
    var v148: array<vec2<u32>, 2>;
    var v149: vec2<u32>;
    var v150: vec2<u32>;
    var v151: array<vec2<u32>, 2>;
    var v152: array<vec2<u32>, 2>;
    var v153: array<vec2<u32>, 2>;
    var v154: array<vec2<u32>, 2>;
    var v155: array<vec2<u32>, 2>;
    var v156: vec2<bool>;
    var v157: array<vec2<u32>, 2>;
    var v158: vec2<bool>;
    var v159: array<vec2<u32>, 2>;
    var v160: vec2<u32>;
    var v161: vec2<u32>;
    var v162: vec2<u32>;
    var v163: vec2<u32>;
    var v164: vec2<u32>;
    var v165: vec2<u32>;
    var v166: vec2<u32>;
    var v167: bool;
    var v168: vec2<u32>;
    var v169: vec2<u32>;
    var v170: vec2<u32>;
    var v171: vec2<u32>;
    var v172: u32;
    var v173: u32;
    var v174: vec2<u32>;
    var v175: vec2<u32>;
    var v176: u32;
    var v177: vec2<u32>;
    var v178: vec2<u32>;
    var v179: vec2<u32>;
    var v180: u32;
    var v181: u32;
    var v182: u32;
    var v183: bool;
    var v184: u32;
    var v185: vec2<u32>;
    var v186: u32;
    var v187: u32;
    var v188: u32;
    var v189: vec2<u32>;
    var v190: bool;
    var v191: vec2<u32>;
    var v192: vec2<u32>;
    var v193: vec2<u32>;
    var v194: vec2<u32>;
    var v195: u32;
    var v196: vec2<u32>;
    var v197: vec2<u32>;
    var v198: vec2<u32>;
    var v199: vec2<u32>;
    var v200: vec2<u32>;
    var v201: vec2<u32>;
    var v202: bool;
    var v203: vec2<u32>;
    var v204: bool;
    var v205: vec2<u32>;
    var v206: bool;
    var v207: bool;
    var v208: bool;
    var v209: vec2<u32>;
    var v210: vec2<u32>;
    var v211: vec2<u32>;
    var v212: vec2<u32>;
    var v213: u32;
    var v214: u32;
    var v215: u32;
    var v216: bool;
    var v217: u32;
    var v218: vec2<u32>;
    var v219: u32;
    var v220: u32;
    var v221: u32;
    var v222: vec2<u32>;
    var v223: bool;
    var v224: vec2<u32>;
    var v225: vec2<u32>;
    var v226: vec2<u32>;
    var v227: vec2<u32>;
    var v228: u32;
    var v229: vec2<u32>;
    var v230: vec2<u32>;
    var v231: vec2<u32>;
    var v232: vec2<u32>;
    var v233: vec2<u32>;
    var v234: vec2<u32>;
    var v235: bool;
    var v236: vec2<u32>;
    var v237: bool;
    var v238: vec2<u32>;
    var v239: bool;
    var v240: bool;
    var v241: vec2<u32>;
    var v242: vec2<u32>;
    var v243: vec2<u32>;
    var v244: vec2<u32>;
    var v245: vec2<u32>;
    var v246: vec2<u32>;
    var v247: vec2<u32>;
    var v248: vec2<u32>;
    var v249: vec2<u32>;
    var v250: vec2<u32>;
    var v251: vec2<u32>;
    var v252: vec2<u32>;
    var v253: vec2<u32>;
    var v254: vec2<u32>;
    var v255: vec2<u32>;
    var v256: vec2<u32>;
    var v257: vec2<u32>;
    var v258: vec2<u32>;
    var v259: vec2<u32>;
    var v260: vec2<u32>;
    var v261: vec2<u32>;
    var v262: vec2<u32>;
    var v263: vec2<u32>;
    var v264: vec2<u32>;
    var v265: vec2<u32>;
    var v266: bool;
    var v267: vec2<u32>;
    var v268: vec2<u32>;
    var v269: u32;
    var v270: vec2<u32>;
    var v271: vec2<u32>;
    var v272: vec2<u32>;
    var v273: vec2<u32>;
    var v274: vec2<u32>;
    var v275: vec2<u32>;
    var v276: vec2<u32>;
    var v277: vec2<u32>;
    var v278: vec2<u32>;
    var v279: vec2<u32>;
    var v280: vec2<u32>;
    var v281: vec2<u32>;
    var v282: vec2<u32>;
    var v283: vec2<u32>;
    var v284: vec2<u32>;
    var v285: vec2<u32>;
    var v286: vec2<u32>;
    var v287: u32;
    var v288: u32;
    var v289: vec2<u32>;
    var v290: u32;
    var v291: vec2<u32>;
    var v292: u32;
    var v293: u32;
    var v294: u32;
    var v295: array<vec2<u32>, 2>;
    var v296: array<vec2<u32>, 2>;
    var v297: vec2<u32>;
    var v298: vec2<u32>;
    var v299: u32;
    var v300: u32;
    var v301: u32;
    var v302: u32;
    var v303: u32;
    var v304: u32;
    var v305: vec2<u32>;
    var v306: bool;
    var v307: vec2<u32>;
    var v308: bool;
    var v309: vec2<u32>;
    var v310: vec2<u32>;
    var v311: vec2<u32>;
    var v312: vec2<u32>;
    var v313: vec2<u32>;
    var v314: vec2<u32>;
    var v315: vec2<u32>;
    var v316: vec2<u32>;
    var v317: vec2<u32>;
    var v318: bool;
    var v319: vec2<u32>;
    var v320: vec2<u32>;
    var v321: vec2<u32>;
    var v322: vec2<u32>;
    var v323: vec2<u32>;
    var v324: vec2<u32>;
    var v325: vec2<u32>;
    var v326: vec2<u32>;
    var v327: vec2<u32>;
    var v328: vec2<u32>;
    var v329: vec2<u32>;
    var v330: vec2<u32>;
    var v331: bool;
    var v332: bool;
    var v333: vec2<u32>;
    var v334: vec2<u32>;
    var v335: bool;
    var v336: bool;
    var v337: bool;
    var v338: vec2<u32>;
    var v339: vec2<u32>;
    var v340: bool;
    var v341: bool;
    var v342: bool;
    var v343: vec2<u32>;
    var v344: vec2<u32>;
    var v345: bool;
    var v346: vec2<u32>;
    var v347: bool;
    var v348: vec2<u32>;
    var v349: vec2<u32>;
    var v350: vec2<u32>;
    var v351: vec2<u32>;
    var v352: u32;
    var v353: vec2<u32>;
    var v354: vec2<u32>;
    var v355: vec2<u32>;
    var v356: u32;
    var v357: u32;
    var v358: vec2<u32>;
    var v359: vec2<u32>;
    var v360: vec2<u32>;
    var v361: vec2<u32>;
    var v362: vec2<u32>;
    var v363: vec2<u32>;
    var v364: u32;
    var v365: vec2<u32>;
    var v366: u32;
    var v367: vec2<u32>;
    var v368: vec2<u32>;
    var v369: vec2<u32>;
    var v370: vec2<u32>;
    var v371: bool;
    var v372: bool;
    var v373: bool;
    var v374: bool;
    var v375: bool;
    var v376: u32;
    var v377: u32;
    var v378: vec2<u32>;
    var v379: vec2<u32>;
    var v380: vec2<u32>;
    var v381: vec2<u32>;
    var v382: vec2<u32>;
    var v383: vec2<u32>;
    var v384: vec2<u32>;
    var v385: vec2<u32>;
    var v386: vec2<u32>;
    var v387: vec2<u32>;
    var v388: vec2<u32>;
    var v389: vec2<u32>;
    var v390: u32;
    var v391: u32;
    var v392: vec2<u32>;
    var v393: vec2<u32>;
    var v394: vec2<u32>;
    var v395: u32;
    var v396: u32;
    var v397: u32;
    var v398: bool;
    var v399: u32;
    var v400: vec2<u32>;
    var v401: u32;
    var v402: u32;
    var v403: u32;
    var v404: vec2<u32>;
    var v405: bool;
    var v406: vec2<u32>;
    var v407: vec2<u32>;
    var v408: vec2<u32>;
    var v409: vec2<u32>;
    var v410: u32;
    var v411: vec2<u32>;
    var v412: vec2<u32>;
    var v413: vec2<u32>;
    var v414: vec2<u32>;
    var v415: vec2<u32>;
    var v416: vec2<u32>;
    var v417: vec2<u32>;
    var v418: bool;
    var v419: vec2<u32>;
    var v420: vec2<u32>;
    var v421: bool;
    var v422: vec2<u32>;
    var v423: vec2<u32>;
    var v424: bool;
    var v425: bool;
    var v426: bool;
    var v427: vec2<u32>;
    var v428: vec2<u32>;
    var v429: bool;
    var v430: vec2<u32>;
    var v431: bool;
    var v432: vec2<u32>;
    var v433: bool;
    var v434: vec2<u32>;
    var v435: vec2<u32>;
    var v436: vec2<u32>;
    var v437: bool;
    var v438: vec2<u32>;
    var v439: vec2<u32>;
    var v440: vec2<u32>;
    var v441: vec2<u32>;
    var v442: vec2<u32>;
    var v443: vec2<u32>;
    var v444: vec2<u32>;
    var v445: vec2<u32>;
    var v446: vec2<u32>;
    var v447: vec2<u32>;
    var v448: vec2<u32>;
    var v449: vec2<u32>;
    var v450: vec2<u32>;
    var v451: vec2<u32>;
    var v452: vec2<u32>;
    var v453: vec2<u32>;
    var v454: vec2<u32>;
    var v455: vec2<u32>;
    var v456: vec2<u32>;
    var v457: vec2<u32>;
    var v458: vec2<u32>;
    var v459: vec2<u32>;
    var v460: vec2<u32>;
    var v461: vec2<u32>;
    var v462: vec2<u32>;
    var v463: vec2<u32>;
    var v464: vec2<u32>;
    var v465: vec2<u32>;
    var v466: vec2<u32>;
    var v467: u32;
    var v468: vec2<u32>;
    var v469: vec2<u32>;
    var v470: u32;
    var v471: u32;
    var v472: u32;
    var v473: u32;
    var v474: u32;
    var v475: u32;
    var v476: u32;
    var v477: vec2<u32>;
    var v478: u32;
    var v479: vec2<u32>;
    var v480: u32;
    var v481: vec2<u32>;
    var v482: u32;
    var v483: vec2<u32>;
    var v484: u32;
    var v485: vec2<u32>;
    var v486: u32;
    var v487: vec2<u32>;
    var v488: u32;
    var v489: u32;
    var v490: vec2<u32>;
    var v491: u32;
    var v492: u32;
    var v493: vec2<u32>;
    var v494: u32;
    var v495: u32;
    var v496: vec2<u32>;
    var v497: u32;
    var v498: u32;
    var v499: vec2<u32>;
    var v500: u32;
    var v501: u32;
    var v502: u32;
    var v503: bool;
    var v504: bool;
    var v505: bool;
    var v506: u32;
    var v507: u32;
    var v508: u32;
    var v509: u32;
    var v510: u32;
    var v511: u32;
    var v512: u32;
    var v513: u32;
    var v514: u32;
    var v515: u32;
    var v516: u32;
    var v517: u32;
    var v518: u32;
    var v519: u32;
    var v520: u32;
    var v521: u32;
    var v522: u32;
    var v523: u32;
    var v524: u32;
    var v525: u32;
    var v526: u32;
    var v527: vec2<u32>;
    var v528: vec2<u32>;
    var v529: vec2<u32>;
    var v530: vec2<u32>;
    var v531: u32;
    var v532: u32;
    var v533: vec2<u32>;
    var v534: vec2<u32>;
    var v535: vec2<u32>;
    var v536: vec2<u32>;
    var v537: vec2<u32>;
    var v538: vec2<u32>;
    var v539: u32;
    var v540: vec2<u32>;
    var v541: vec2<u32>;
    var v542: vec2<u32>;
    var v543: vec2<u32>;
    var v544: u32;
    var v545: vec2<u32>;
    var v546: vec2<u32>;
    var v547: vec2<u32>;
    var v548: vec2<u32>;
    var v549: vec2<u32>;
    var v550: vec2<u32>;
    var v551: vec2<u32>;
    var v552: vec2<u32>;
    var v553: u32;
    var v554: vec2<u32>;
    var v555: vec2<u32>;
    var v556: vec2<u32>;
    var v557: vec2<u32>;
    var v558: u32;
    var v559: bool;
    var v560: vec2<u32>;
    var v561: vec2<u32>;
    var v562: u32;
    var v563: u32;
    var v564: u32;
    var v565: u32;
    var v566: vec2<u32>;
    var v567: vec2<u32>;
    var v568: vec2<u32>;
    var v569: vec2<u32>;
    var v570: u32;
    var v571: u32;
    var v572: vec2<u32>;
    var v573: vec2<u32>;
    var v574: vec2<u32>;
    var v575: vec2<u32>;
    var v576: vec2<u32>;
    var v577: u32;
    var v578: vec2<u32>;
    var v579: vec2<u32>;
    var v580: vec2<u32>;
    var v581: u32;
    var v582: vec2<u32>;
    var v583: u32;
    var v584: u32;
    var v585: vec2<u32>;
    var v586: vec2<u32>;
    var v587: vec2<u32>;
    var v588: vec2<u32>;
    var v589: vec2<u32>;
    var v590: u32;
    var v591: vec2<u32>;
    var v592: vec2<u32>;
    var v593: u32;
    var v594: u32;
    var v595: vec2<u32>;
    var v596: u32;
    var v597: vec2<u32>;
    var v598: u32;
    var v599: vec2<u32>;
    var v600: u32;
    var v601: u32;
    var v602: vec2<u32>;
    var v603: vec2<u32>;
    var v604: vec2<u32>;
    var v605: vec2<u32>;
    var v606: vec2<u32>;
    var v607: vec2<u32>;
    var v608: vec2<u32>;
    var v609: array<vec2<u32>, 2>;
    var v610: array<vec2<u32>, 2>;
    var v611: array<vec2<u32>, 2>;
    var v612: array<vec2<u32>, 2>;
    var v613: array<vec2<u32>, 2>;
    var v614: array<vec2<u32>, 2>;
    var v615: array<vec2<u32>, 2>;
    var v616: array<vec2<u32>, 2>;
    var v617: array<vec2<u32>, 2>;
    var v618: array<vec2<u32>, 2>;
    var v619: array<vec2<u32>, 2>;
    var v620: array<vec2<u32>, 2>;
    var v621: array<vec2<u32>, 2>;
    var v622: array<vec2<u32>, 2>;
    var v623: array<vec2<u32>, 2>;
    var v624: vec2<u32>;
    var v625: vec2<u32>;
    var v626: vec2<u32>;
    var v627: vec2<u32>;
    var v628: vec2<u32>;
    var v629: vec2<u32>;
    var v630: vec2<u32>;
    var v631: vec2<u32>;
    var v632: vec2<u32>;
    var v633: vec2<u32>;
    var v634: vec2<u32>;
    var v635: vec2<u32>;
    var v636: vec2<u32>;
    var v637: vec2<u32>;
    var v638: vec2<u32>;
    var v639: vec2<u32>;
    var v640: vec2<u32>;
    var v641: vec2<u32>;
    var v642: vec2<u32>;
    var v643: vec2<u32>;
    var v644: vec2<u32>;
    var v645: vec2<u32>;
    var v646: vec2<u32>;
    var v647: vec2<u32>;
    var v648: vec2<u32>;
    var v649: vec2<u32>;
    var v650: vec2<u32>;
    var v651: vec2<u32>;
    var v652: vec2<u32>;
    var v653: vec2<u32>;
    var v654: vec2<u32>;
    var v655: vec2<u32>;
    var v656: bool;
    var v657: vec2<u32>;
    var v658: bool;
    var v659: vec2<u32>;
    var v660: vec2<u32>;
    var v661: vec2<u32>;
    var v662: bool;
    var v663: vec2<u32>;
    var v664: vec2<u32>;
    var v665: vec2<u32>;
    var v666: vec2<u32>;
    var v667: vec2<u32>;
    var v668: vec2<u32>;
    var v669: vec2<u32>;
    var v670: bool;
    var v671: vec2<u32>;
    var v672: bool;
    var v673: vec2<u32>;
    var v674: vec2<u32>;
    var v675: f32;
    var v676: u32;
    var v677: vec2<u32>;
    var v678: vec2<u32>;
    var v679: vec2<u32>;
    var v680: vec2<u32>;
    var v681: vec2<u32>;
    var v682: vec2<u32>;
    var v683: vec2<u32>;
    var v684: vec2<u32>;
    var v685: bool;
    var v686: u32;
    var v687: u32;
    var v688: vec2<u32>;
    var v689: vec2<u32>;
    var v690: vec2<u32>;
    var v691: vec2<u32>;
    var v692: vec2<u32>;
    var v693: vec2<u32>;
    var v694: vec2<u32>;
    var v695: vec2<u32>;
    var v696: vec2<u32>;
    var v697: vec2<u32>;
    var v698: vec2<u32>;
    var v699: vec2<u32>;
    var v700: vec2<u32>;
    var v701: vec2<u32>;
    var v702: vec2<u32>;
    var v703: vec2<u32>;
    var v704: u32;
    var v705: vec2<u32>;
    var v706: vec2<u32>;
    var v707: vec2<u32>;
    var v708: vec2<u32>;
    var v709: vec2<u32>;
    var v710: vec2<u32>;
    var v711: vec2<u32>;
    var v712: vec2<u32>;
    var v713: vec2<u32>;
    var v714: vec2<u32>;
    var v715: vec2<u32>;
    var v716: vec2<u32>;
    var v717: vec2<u32>;
    var v718: vec2<u32>;
    var v719: vec2<u32>;
    var v720: vec2<u32>;
    var v721: vec2<u32>;
    var v722: vec2<u32>;
    var v723: vec2<u32>;
    var v724: vec2<u32>;
    var v725: vec2<u32>;
    var v726: vec2<u32>;
    var v727: vec2<u32>;
    var v728: vec2<u32>;
    var v729: vec2<u32>;
    var v730: vec2<u32>;
    var v731: vec2<u32>;
    var v732: u32;
    var v733: bool;
    var v734: vec2<u32>;
    var v735: vec2<u32>;
    var v736: u32;
    var v737: u32;
    var v738: vec2<u32>;
    var v739: vec2<u32>;
    var v740: vec2<u32>;
    var v741: vec2<u32>;
    var v742: bool;
    var v743: vec2<u32>;
    var v744: vec2<u32>;
    var v745: vec2<u32>;
    var v746: vec2<u32>;
    var v747: bool;
    var v748: vec2<u32>;
    var v749: vec2<u32>;
    var v750: vec2<u32>;
    var v751: vec2<u32>;
    var v752: vec2<u32>;
    var v753: vec2<u32>;
    var v754: vec2<u32>;
    var v755: u32;
    var v756: vec2<u32>;
    var v757: u32;
    var v758: u32;
    var v759: bool;
    loop {
        v0 = select(0u, gid[min(0u, 2u)], 0u < 3u);
        v1 = bitcast<i32>(v0) < bitcast<i32>(16u);
        if (v1) {
            v2 = a1 + 8u;
            v4 = v0;
            loop {
                loop {
                    v5 = v4 << 1u;
                    v6 = a0 + v5 * 8u;
                    v7 = array<vec2<u32>, 2>(vec2<u32>(buf0[(((v6 + 0u)) - 4096u) >> 2u], buf0[((((v6 + 0u) + 4u)) - 4096u) >> 2u]), vec2<u32>(buf0[(((v6 + 8u)) - 4096u) >> 2u], buf0[((((v6 + 8u) + 4u)) - 4096u) >> 2u]));
                    v8 = vec2<u32>(buf2[((a2) - 4096u) >> 2u], buf2[(((a2 + 4u)) - 4096u) >> 2u]);
                    v9[0] = vec2<u32>(v7[0].x, v7[0].y & 0x7fffffffu);
                    v9[1] = vec2<u32>(v7[1].x, v7[1].y & 0x7fffffffu);
                    v10 = v7[0];
                    v11 = !(f64_is_nan(v10) | f64_is_nan(vec2<u32>(0u, 0u)));
                    if (v11) {
                        v12 = v9[0];
                        v13 = v12;
                        v14 = lshr64(v13, vec2<u32>(52u, 0u).x);
                        v15 = v14.x;
                        v16 = v15 > 522u;
                        v17 = v9[1];
                        v18 = v17;
                        v19 = lshr64(v18, vec2<u32>(52u, 0u).x);
                        v20 = v19.x;
                        v21 = v20 > 522u;
                        v22 = (v16 & v21);
                        v23 = v15 < 1524u;
                        v24 = v20 < 1524u;
                        v25 = (v23 & v24);
                        v26 = select(vec2<u32>(0u, 443547648u), vec2<u32>(0u, 1072693248u), v25);
                        v27 = select(vec2<u32>(0u, 1701838848u), v26, v22);
                        v28 = f64_mul(v12, v27);
                        v29 = f64_mul(v17, v27);
                        v30 = f64_mul(v29, v29);
                        v31 = f64_fma(v28, v28, v30);
                        v32 = f64_sqrt(v31);
                        v33 = v15 - v20;
                        v34 = u32(abs(bitcast<i32>(v33)));
                        v35 = v34 < 55u;
                        v36 = v7[1];
                        v37 = !(f64_is_nan(v36) | f64_is_nan(vec2<u32>(0u, 0u)));
                        v38 = select(vec2<u32>(0u, 1701838848u), vec2<u32>(0u, 1072693248u), v25);
                        v39 = select(vec2<u32>(0u, 443547648u), v38, v22);
                        v40 = f64_mul(v39, v32);
                        v41 = f64_add(v12, v17);
                        v42 = select(v41, v40, v35);
                        if (v37) {
                            v43 = v42;
                            break;
                        } else {
                        }
                    } else {
                    }
                    v43 = vec2<u32>(0u, 2146959360u);
                    break;
                }
                v44[0] = f64_eq(v9[0], array<vec2<u32>, 2>(vec2<u32>(0u, 2146435072u), vec2<u32>(0u, 2146435072u))[0]);
                v44[1] = f64_eq(v9[1], array<vec2<u32>, 2>(vec2<u32>(0u, 2146435072u), vec2<u32>(0u, 2146435072u))[1]);
                v45 = v44[0];
                v46 = v44[1];
                v47 = (v45 | v46);
                v48 = select(v43, vec2<u32>(0u, 2146435072u), v47);
                v49 = (f64_lt(vec2<u32>(0u, 1072566186u), v48) | f64_eq(v48, vec2<u32>(0u, 1072566186u)));
                v50 = (f64_lt(v48, vec2<u32>(0u, 1072760876u)) | f64_eq(v48, vec2<u32>(0u, 1072760876u)));
                v51 = (v49 & v50);
                v52 = f64_add(v48, vec2<u32>(0u, 3220176896u));
                v53 = f64_add(v52, vec2<u32>(0u, 1073741824u));
                v54 = f64_div(v52, v53);
                v55 = f64_add(v54, v54);
                v56 = f64_mul(v55, v55);
                v57 = f64_mul(v55, v56);
                v58 = f64_fma(v56, vec2<u32>(3361603568u, 1060929588u), vec2<u32>(133255583u, 1063405859u));
                v59 = f64_fma(v56, v58, vec2<u32>(2579154644u, 1065982361u));
                v60 = f64_fma(v56, v59, vec2<u32>(1431655654u, 1068848469u));
                v61 = vec2<u32>(v54.x, v54.y ^ 0x80000000u);
                v62 = f64_mul(v52, v61);
                v63 = f64_fma(v57, v60, v62);
                v64 = v52;
                v65 = v64 & vec2<u32>(0u, 4294967295u);
                v66 = v65;
                v67 = f64_sub(v52, v66);
                v68 = f64_add(v67, v63);
                v69 = f64_mul(v68, vec2<u32>(401652670u, 1053501788u));
                v70 = f64_fma(v66, vec2<u32>(401652670u, 1053501788u), v69);
                v71 = f64_fma(v68, vec2<u32>(0u, 1073157444u), v70);
                v72 = f64_fma(v66, vec2<u32>(0u, 1073157444u), v71);
                v73 = v48;
                v74 = v73 | vec2<u32>(0u, 63963136u);
                v75 = v74;
                v76 = f64_add(v75, vec2<u32>(0u, 2211446784u));
                v77 = v76;
                v78 = ult64(v73, vec2<u32>(0u, 1048576u));
                v79 = select(v73, v77, v78);
                v80 = v79;
                v81 = v80[1];
                v82 = v81 >> 20u;
                v83 = v82 & 2047u;
                v84 = select(4294966273u, 4294966213u, v78);
                v85 = v83 + v84;
                v86 = v79 & vec2<u32>(4294967295u, 1048575u);
                v87 = v86 | vec2<u32>(0u, 1071644672u);
                v88 = v87;
                v89 = u32(bitcast<i32>(v81) >> 13u);
                v90 = v89 >> 1u;
                v91 = v90 & 63u;
                v92 = v91 | 64u;
                v93 = v89 & 1u;
                v94 = v92 + v93;
                v95 = v94 + 4294967232u;
                v96 = 4096u + 0u + v95 * 8u;
                v97 = vec2<u32>(glob0[((v96) - 4096u) >> 2u], glob0[(((v96 + 4u)) - 4096u) >> 2u]);
                v98 = 4096u + 0u + v95 * 8u;
                v99 = vec2<u32>(glob1[((v98) - 4096u) >> 2u], glob1[(((v98 + 4u)) - 4096u) >> 2u]);
                v100 = f64_from_u64(vec2<u32>(v94, 0u));
                v101 = f64_mul(v100, vec2<u32>(0u, 1065353216u));
                v102 = f64_sub(v88, v101);
                v103 = f64_fma(v102, vec2<u32>(0u, 1071644672u), v101);
                v104 = f64_div(v102, v103);
                v105 = f64_mul(v104, v104);
                v106 = f64_fma(v105, vec2<u32>(1004095297u, 1063405890u), vec2<u32>(2575720158u, 1065982361u));
                v107 = f64_fma(v105, v106, vec2<u32>(1431655767u, 1068848469u));
                v108 = f64_mul(v105, v107);
                v109 = f64_fma(v104, v108, v104);
                v110 = f64_add(v99, v109);
                v111 = f64_from_i64(vec2<u32>(bitcast<u32>(bitcast<i32>(v85)), select(0u, 0xffffffffu, bitcast<i32>(v85) < 0i)));
                v112 = f64_fma(v97, vec2<u32>(0u, 1073157444u), v111);
                v113 = f64_mul(v110, vec2<u32>(401652670u, 1053501788u));
                v114 = f64_fma(v97, vec2<u32>(401652670u, 1053501788u), v113);
                v115 = f64_fma(v110, vec2<u32>(0u, 1073157444u), v114);
                v116 = f64_add(v112, v115);
                v117 = select(v116, v72, v51);
                v118 = vec2<u32>(v48.x, v48.y & 0x7fffffffu);
                v119 = !f64_eq(v118, vec2<u32>(0u, 2146435072u));
                v120 = (f64_lt(vec2<u32>(0u, 0u), v48) | f64_eq(v48, vec2<u32>(0u, 0u)));
                v121 = f64_eq(v48, vec2<u32>(0u, 0u));
                v122 = f64_mul(v117, vec2<u32>(4277811695u, 1072049730u));
                v123 = select(vec2<u32>(0u, 2146435072u), v122, v119);
                v124 = select(vec2<u32>(0u, 2146959360u), v123, v120);
                v125 = select(v124, vec2<u32>(0u, 4293918720u), v121);
                v126 = select(false, v46, v45);
                v127[0] = v7[0];
                v127[1] = v7[1];
                v128[0] = f64_mul(v7[0], array<vec2<u32>, 2>(vec2<u32>(0u, 1128267776u), vec2<u32>(0u, 1128267776u))[0]);
                v128[1] = f64_mul(v7[1], array<vec2<u32>, 2>(vec2<u32>(0u, 1128267776u), vec2<u32>(0u, 1128267776u))[1]);
                v129[0] = vec2<u32>(array<vec2<u32>, 2>(vec2<u32>(0u, 2146435072u), vec2<u32>(0u, 2146435072u))[0].x, (array<vec2<u32>, 2>(vec2<u32>(0u, 2146435072u), vec2<u32>(0u, 2146435072u))[0].y & 0x7fffffffu) | (v7[0].y & 0x80000000u));
                v129[1] = vec2<u32>(array<vec2<u32>, 2>(vec2<u32>(0u, 2146435072u), vec2<u32>(0u, 2146435072u))[1].x, (array<vec2<u32>, 2>(vec2<u32>(0u, 2146435072u), vec2<u32>(0u, 2146435072u))[1].y & 0x7fffffffu) | (v7[1].y & 0x80000000u));
                v130[0] = f64_eq(v7[0], array<vec2<u32>, 2>()[0]);
                v130[1] = f64_eq(v7[1], array<vec2<u32>, 2>()[1]);
                v131[0] = lshr64(v127[0], array<vec2<u32>, 2>(vec2<u32>(52u, 0u), vec2<u32>(52u, 0u))[0].x);
                v131[1] = lshr64(v127[1], array<vec2<u32>, 2>(vec2<u32>(52u, 0u), vec2<u32>(52u, 0u))[1].x);
                v132[0] = (v131[0].x & 65535u);
                v132[1] = (v131[1].x & 65535u);
                v133 = v132 & vec2<u32>(2047u, 2047u);
                v134 = v133[0];
                v135 = v134 > 1020u;
                v136 = v133[1];
                v137 = v136 > 1020u;
                v138 = select(v137, true, v135);
                v139[0] = v128[0];
                v139[1] = v128[1];
                v140[0] = lshr64(v139[0], array<vec2<u32>, 2>(vec2<u32>(52u, 0u), vec2<u32>(52u, 0u))[0].x);
                v140[1] = lshr64(v139[1], array<vec2<u32>, 2>(vec2<u32>(52u, 0u), vec2<u32>(52u, 0u))[1].x);
                v141[0] = v140[0].x;
                v141[1] = v140[1].x;
                v142 = v141 & vec2<u32>(2047u, 2047u);
                v143 = v142 + vec2<u32>(4294967243u, 4294967243u);
                v144 = v133 == vec2<u32>();
                v145 = v133;
                v146 = select(v145, v143, v144);
                v147[0] = select(v7[0], v128[0], v144[0]);
                v147[1] = select(v7[1], v128[1], v144[1]);
                v148[0] = v147[0];
                v148[1] = v147[1];
                v149 = v146 + vec2<u32>(1024u, 1024u);
                v150 = min(v149, vec2<u32>(2047u, 2047u));
                v151[0] = v148[0] & array<vec2<u32>, 2>(vec2<u32>(4294967295u, 2148532223u), vec2<u32>(4294967295u, 2148532223u))[0];
                v151[1] = v148[1] & array<vec2<u32>, 2>(vec2<u32>(4294967295u, 2148532223u), vec2<u32>(4294967295u, 2148532223u))[1];
                v152[0] = vec2<u32>(v150[0], 0u);
                v152[1] = vec2<u32>(v150[1], 0u);
                v153[0] = shl64(v152[0], array<vec2<u32>, 2>(vec2<u32>(52u, 0u), vec2<u32>(52u, 0u))[0].x);
                v153[1] = shl64(v152[1], array<vec2<u32>, 2>(vec2<u32>(52u, 0u), vec2<u32>(52u, 0u))[1].x);
                v154[0] = v153[0] | v151[0];
                v154[1] = v153[1] | v151[1];
                v155[0] = v154[0];
                v155[1] = v154[1];
                v156 = v149 > vec2<u32>(2046u, 2046u);
                v157[0] = select(v155[0], v129[0], v156[0]);
                v157[1] = select(v155[1], v129[1], v156[1]);
                v158 = (v130 | v44);
                v159[0] = select(v157[0], v7[0], v158[0]);
                v159[1] = select(v157[1], v7[1], v158[1]);
                v160 = v159[0];
                v161 = select(v160, v10, v138);
                v162 = vec2<u32>(v161.x, v161.y & 0x7fffffffu);
                v163 = v159[1];
                v164 = v7[1];
                v165 = select(v163, v164, v138);
                v166 = vec2<u32>(v165.x, v165.y & 0x7fffffffu);
                v167 = f64_lt(v162, v166);
                v168 = select(v166, v162, v167);
                v169 = select(v162, v166, v167);
                v170 = v169;
                v171 = lshr64(v170, vec2<u32>(52u, 0u).x);
                v172 = v171.x;
                v173 = 1023u - v172;
                v174 = v168;
                v175 = lshr64(v174, vec2<u32>(52u, 0u).x);
                v176 = v175.x;
                v177 = f64_mul(v168, vec2<u32>(0u, 1128267776u));
                v178 = v177;
                v179 = lshr64(v178, vec2<u32>(52u, 0u).x);
                v180 = v179.x;
                v181 = v180 & 2047u;
                v182 = v181 + 4294967243u;
                v183 = v176 == 0u;
                v184 = select(v176, v182, v183);
                v185 = select(v174, v178, v183);
                v186 = v184 + v173;
                v187 = u32(max(bitcast<i32>(v186), bitcast<i32>(4294965249u)));
                v188 = u32(min(bitcast<i32>(v187), bitcast<i32>(2047u)));
                v189 = v185 & vec2<u32>(4294967295u, 2148532223u);
                v190 = bitcast<i32>(v186) > bitcast<i32>(0u);
                if (v190) {
                    v191 = vec2<u32>(v188, 0u);
                    v192 = shl64(v191, vec2<u32>(52u, 0u).x);
                    v193 = v192 | v189;
                    v194 = v193;
                    v201 = v194;
                } else {
                    v195 = v188 + 53u;
                    v196 = vec2<u32>(v195, 0u);
                    v197 = shl64(v196, vec2<u32>(52u, 0u).x);
                    v198 = v197 | v189;
                    v199 = v198;
                    v200 = f64_mul(v199, vec2<u32>(0u, 1017118720u));
                    v201 = v200;
                }
                v202 = bitcast<i32>(v186) > bitcast<i32>(2046u);
                v203 = select(v201, vec2<u32>(0u, 2146435072u), v202);
                v204 = bitcast<i32>(v186) < bitcast<i32>(4294967243u);
                v205 = select(v203, vec2<u32>(0u, 0u), v204);
                v206 = v172 == 1023u;
                v207 = ((!((v168.y >> 31u) != 0u) && ((v168.y >> 20u) & 0x7ffu) == 0u && (((v168.y & 0xfffffu) | v168.x) == 0u)) || (!((v168.y >> 31u) != 0u) && ((v168.y >> 20u) & 0x7ffu) == 0x7ffu && (((v168.y & 0xfffffu) | v168.x) == 0u)));
                v208 = (v207 | v206);
                v209 = select(v205, v168, v208);
                v210 = f64_mul(v169, vec2<u32>(0u, 1128267776u));
                v211 = v210;
                v212 = lshr64(v211, vec2<u32>(52u, 0u).x);
                v213 = v212.x;
                v214 = v213 & 2047u;
                v215 = v214 + 4294967243u;
                v216 = v172 == 0u;
                v217 = select(v172, v215, v216);
                v218 = select(v170, v211, v216);
                v219 = v217 + v173;
                v220 = u32(max(bitcast<i32>(v219), bitcast<i32>(4294965249u)));
                v221 = u32(min(bitcast<i32>(v220), bitcast<i32>(2047u)));
                v222 = v218 & vec2<u32>(4294967295u, 2148532223u);
                v223 = bitcast<i32>(v219) > bitcast<i32>(0u);
                if (v223) {
                    v224 = vec2<u32>(v221, 0u);
                    v225 = shl64(v224, vec2<u32>(52u, 0u).x);
                    v226 = v225 | v222;
                    v227 = v226;
                    v234 = v227;
                } else {
                    v228 = v221 + 53u;
                    v229 = vec2<u32>(v228, 0u);
                    v230 = shl64(v229, vec2<u32>(52u, 0u).x);
                    v231 = v230 | v222;
                    v232 = v231;
                    v233 = f64_mul(v232, vec2<u32>(0u, 1017118720u));
                    v234 = v233;
                }
                v235 = bitcast<i32>(v219) > bitcast<i32>(2046u);
                v236 = select(v234, vec2<u32>(0u, 2146435072u), v235);
                v237 = bitcast<i32>(v219) < bitcast<i32>(4294967243u);
                v238 = select(v236, vec2<u32>(0u, 0u), v237);
                v239 = ((!((v169.y >> 31u) != 0u) && ((v169.y >> 20u) & 0x7ffu) == 0u && (((v169.y & 0xfffffu) | v169.x) == 0u)) || (!((v169.y >> 31u) != 0u) && ((v169.y >> 20u) & 0x7ffu) == 0x7ffu && (((v169.y & 0xfffffu) | v169.x) == 0u)));
                v240 = (v239 | v206);
                v241 = select(v238, v169, v240);
                v242 = f64_div(v168, v169);
                v243 = f64_mul(v242, v242);
                v244 = vec2<u32>(v242.x, v242.y ^ 0x80000000u);
                v245 = f64_mul(v243, v244);
                v246 = vec2<u32>(v243.x, v243.y ^ 0x80000000u);
                v247 = f64_fma(v246, vec2<u32>(2463584706u, 1068960817u), vec2<u32>(968410347u, 1069314440u));
                v248 = f64_fma(v246, v247, vec2<u32>(2193452001u, 1069697316u));
                v249 = f64_fma(v246, v248, vec2<u32>(2576761763u, 1070176665u));
                v250 = f64_fma(v246, v249, vec2<u32>(1431655736u, 1070945621u));
                v251 = vec2<u32>(v169.x, v169.y ^ 0x80000000u);
                v252 = v242;
                v253 = v252 & vec2<u32>(0u, 4294967295u);
                v254 = v253;
                v255 = f64_sub(v242, v254);
                v256 = v170 & vec2<u32>(0u, 2147483647u);
                v257 = v256;
                v258 = f64_sub(v169, v257);
                v259 = vec2<u32>(v258.x, v258.y ^ 0x80000000u);
                v260 = vec2<u32>(v257.x, v257.y ^ 0x80000000u);
                v261 = f64_fma(v260, v254, v168);
                v262 = f64_fma(v259, v254, v261);
                v263 = f64_fma(v251, v255, v262);
                v264 = f64_div(v263, v169);
                v265 = f64_fma(v245, v250, v264);
                v266 = f64_lt(vec2<u32>(0u, 1068498944u), v242);
                v267 = f64_fma(v242, vec2<u32>(0u, 1081081856u), vec2<u32>(0u, 1071644672u));
                v268 = select(vec2<u32>(2611340116u, 1076928708u), v267, v266);
                v269 = f64_to_i64(v268).x;
                v270 = f64_from_i64(vec2<u32>(bitcast<u32>(bitcast<i32>(v269)), select(0u, 0xffffffffu, bitcast<i32>(v269) < 0i)));
                v271 = f64_mul(v270, vec2<u32>(0u, 1064304640u));
                v272 = vec2<u32>(v271.x, v271.y ^ 0x80000000u);
                v273 = v241;
                v274 = v273 & vec2<u32>(4160749568u, 4294967295u);
                v275 = v274;
                v276 = f64_sub(v241, v275);
                v277 = f64_fma(v272, v275, v209);
                v278 = f64_fma(v272, v276, v277);
                v279 = f64_fma(v271, v209, v241);
                v280 = f64_div(v278, v279);
                v281 = f64_mul(v280, v280);
                v282 = vec2<u32>(v281.x, v281.y ^ 0x80000000u);
                v283 = f64_fma(v282, vec2<u32>(3112185213u, 1070176658u), vec2<u32>(1431636087u, 1070945621u));
                v284 = f64_mul(v281, v283);
                v285 = vec2<u32>(v280.x, v280.y ^ 0x80000000u);
                v286 = f64_fma(v284, v285, v280);
                v287 = v269 + 4294967280u;
                v288 = 4096u + 0u + v287 * 8u;
                v289 = vec2<u32>(glob3[((v288) - 4096u) >> 2u], glob3[(((v288 + 4u)) - 4096u) >> 2u]);
                v290 = 4096u + 0u + v287 * 8u;
                v291 = vec2<u32>(glob2[((v290) - 4096u) >> 2u], glob2[(((v290 + 4u)) - 4096u) >> 2u]);
                v292 = v134;
                v293 = v136;
                v294 = v293 - v292;
                v295[0] = v159[0];
                v295[1] = v159[1];
                v296[0] = lshr64(v295[0], array<vec2<u32>, 2>(vec2<u32>(52u, 0u), vec2<u32>(52u, 0u))[0].x);
                v296[1] = lshr64(v295[1], array<vec2<u32>, 2>(vec2<u32>(52u, 0u), vec2<u32>(52u, 0u))[1].x);
                v297[0] = (v296[0].x & 65535u);
                v297[1] = (v296[1].x & 65535u);
                v298 = v297 & vec2<u32>(2047u, 2047u);
                v299 = v298[0];
                v300 = v299;
                v301 = v298[1];
                v302 = v301;
                v303 = v302 - v300;
                v304 = select(v303, v294, v138);
                v305 = v127[0];
                v306 = slt64(v305, vec2<u32>(0u, 0u));
                v307 = v127[1];
                v308 = slt64(v307, vec2<u32>(0u, 0u));
                v309 = select(vec2<u32>(2134057426u, 1073928572u), vec2<u32>(2134057426u, 3221412220u), v308);
                v310 = select(vec2<u32>(1413754136u, 1072243195u), vec2<u32>(1413754136u, 3219726843u), v308);
                v311 = select(v310, v309, v306);
                v312 = select(vec2<u32>(0u, 0u), v291, v266);
                v313 = f64_sub(vec2<u32>(1413754136u, 1073291771u), v312);
                v314 = select(v312, v313, v167);
                v315 = f64_sub(vec2<u32>(1342177280u, 1074340347u), v314);
                v316 = select(v314, v315, v306);
                v317 = f64_add(v289, v286);
                v318 = f64_lt(v242, vec2<u32>(183607935u, 1045238509u));
                v319 = f64_add(v242, v265);
                v320 = select(v319, v242, v318);
                v321 = select(v320, v317, v266);
                v322 = f64_sub(vec2<u32>(856972295u, 1016178214u), v321);
                v323 = select(v321, v322, v167);
                v324 = f64_sub(vec2<u32>(1629119075u, 1046548660u), v323);
                v325 = select(v323, v324, v306);
                v326 = f64_add(v316, v325);
                v327 = vec2<u32>(v326.x, v326.y ^ 0x80000000u);
                v328 = select(v326, v327, v308);
                v329 = select(v328, v311, v126);
                v330 = select(vec2<u32>(1413754136u, 1074340347u), vec2<u32>(1413754136u, 3221823995u), v308);
                v331 = bitcast<i32>(v304) < bitcast<i32>(4294967240u);
                v332 = (v306 & v331);
                v333 = select(v329, v330, v332);
                v334 = f64_div(v165, v161);
                v335 = bitcast<i32>(v304) < bitcast<i32>(4294967268u);
                v336 = slt64(vec2<u32>(4294967295u, 4294967295u), v305);
                v337 = (v336 & v335);
                v338 = select(v333, v334, v337);
                v339 = select(vec2<u32>(1413754136u, 1073291771u), vec2<u32>(1413754136u, 3220775419u), v308);
                v340 = bitcast<i32>(v304) > bitcast<i32>(56u);
                v341 = v130[0];
                v342 = (v341 | v340);
                v343 = select(v338, v339, v342);
                v344 = select(v164, v330, v306);
                v345 = v130[1];
                v346 = select(v343, v344, v345);
                v347 = !(f64_is_nan(v164) | f64_is_nan(vec2<u32>(0u, 0u)));
                v348 = select(v164, v346, v347);
                v349 = select(v10, v348, v11);
                v350 = f64_mul(v8, v125);
                v351 = f64_mul(v350, vec2<u32>(1697350398u, 1079448903u));
                v352 = f64_to_i64(v351).x;
                v353 = f64_from_i64(vec2<u32>(bitcast<u32>(bitcast<i32>(v352)), select(0u, 0xffffffffu, bitcast<i32>(v352) < 0i)));
                v354 = f64_fma(v353, vec2<u32>(4277796864u, 3213241922u), v350);
                v355 = f64_fma(v353, vec2<u32>(3164486457u, 3172792218u), v354);
                v356 = v352 & 63u;
                v357 = u32(bitcast<i32>(v352) >> 6u);
                v358 = f64_fma(v355, vec2<u32>(381774871u, 1062650220u), vec2<u32>(286331153u, 1065423121u));
                v359 = f64_fma(v355, v358, vec2<u32>(1431655765u, 1067799893u));
                v360 = f64_fma(v355, v359, vec2<u32>(1431655765u, 1069897045u));
                v361 = f64_fma(v355, v360, vec2<u32>(0u, 1071644672u));
                v362 = f64_fma(v355, v361, vec2<u32>(0u, 1072693248u));
                v363 = f64_mul(v355, v362);
                v364 = 4096u + 0u + v356 * 8u;
                v365 = vec2<u32>(glob4[((v364) - 4096u) >> 2u], glob4[(((v364 + 4u)) - 4096u) >> 2u]);
                v366 = 4096u + 0u + v356 * 8u;
                v367 = vec2<u32>(glob5[((v366) - 4096u) >> 2u], glob5[(((v366 + 4u)) - 4096u) >> 2u]);
                v368 = f64_add(v365, v367);
                v369 = f64_fma(v368, v363, v367);
                v370 = f64_add(v365, v369);
                v371 = bitcast<i32>(v357) < bitcast<i32>(4294966274u);
                if (v371) {
                    v375 = false;
                } else {
                    v372 = v357 != 4294966274u;
                    v373 = !f64_lt(v370, vec2<u32>(0u, 1072693248u));
                    v374 = (v372 | v373);
                    v375 = v374;
                }
                v376 = u32(bitcast<i32>(v352) >> 8u);
                v377 = v357 - v376;
                v378 = vec2<u32>(v376, 0u);
                v379 = shl64(v378, vec2<u32>(52u, 0u).x);
                v380 = add64(v379, vec2<u32>(0u, 1072693248u));
                v381 = v380;
                v382 = f64_mul(v370, v381);
                v383 = vec2<u32>(v377, 0u);
                v384 = shl64(v383, vec2<u32>(52u, 0u).x);
                v385 = add64(v384, vec2<u32>(0u, 1072693248u));
                v386 = v385;
                v387 = f64_mul(v382, v386);
                v388 = v370;
                v389 = lshr64(v388, vec2<u32>(52u, 0u).x);
                v390 = v389.x;
                v391 = v390 & 2047u;
                v392 = f64_mul(v370, vec2<u32>(0u, 1128267776u));
                v393 = v392;
                v394 = lshr64(v393, vec2<u32>(52u, 0u).x);
                v395 = v394.x;
                v396 = v395 & 2047u;
                v397 = v396 + 4294967243u;
                v398 = v391 == 0u;
                v399 = select(v391, v397, v398);
                v400 = select(v388, v393, v398);
                v401 = v399 + v357;
                v402 = u32(max(bitcast<i32>(v401), bitcast<i32>(4294965249u)));
                v403 = u32(min(bitcast<i32>(v402), bitcast<i32>(2047u)));
                v404 = v400 & vec2<u32>(4294967295u, 2148532223u);
                v405 = bitcast<i32>(v401) > bitcast<i32>(0u);
                if (v405) {
                    v406 = vec2<u32>(v403, 0u);
                    v407 = shl64(v406, vec2<u32>(52u, 0u).x);
                    v408 = v407 | v404;
                    v409 = v408;
                    v416 = v409;
                } else {
                    v410 = v403 + 53u;
                    v411 = vec2<u32>(v410, 0u);
                    v412 = shl64(v411, vec2<u32>(52u, 0u).x);
                    v413 = v412 | v404;
                    v414 = v413;
                    v415 = f64_mul(v414, vec2<u32>(0u, 1017118720u));
                    v416 = v415;
                }
                v417 = v388 & vec2<u32>(0u, 2147483648u);
                v418 = bitcast<i32>(v401) > bitcast<i32>(2046u);
                v419 = vec2<u32>(vec2<u32>(0u, 2146435072u).x, (vec2<u32>(0u, 2146435072u).y & 0x7fffffffu) | (v370.y & 0x80000000u));
                v420 = select(v416, v419, v418);
                v421 = bitcast<i32>(v401) < bitcast<i32>(4294967243u);
                v422 = v417;
                v423 = select(v420, v422, v421);
                v424 = v352 < 64u;
                v425 = ((((v370.y >> 31u) != 0u) && ((v370.y >> 20u) & 0x7ffu) == 0x7ffu && (((v370.y & 0xfffffu) | v370.x) == 0u)) || (((v370.y >> 31u) != 0u) && ((v370.y >> 20u) & 0x7ffu) == 0u && (((v370.y & 0xfffffu) | v370.x) == 0u)) || (!((v370.y >> 31u) != 0u) && ((v370.y >> 20u) & 0x7ffu) == 0u && (((v370.y & 0xfffffu) | v370.x) == 0u)) || (!((v370.y >> 31u) != 0u) && ((v370.y >> 20u) & 0x7ffu) == 0x7ffu && (((v370.y & 0xfffffu) | v370.x) == 0u)));
                v426 = (v424 | v425);
                v427 = select(v423, v370, v426);
                v428 = select(v387, v427, v375);
                v429 = !(f64_is_nan(v350) | f64_is_nan(vec2<u32>(0u, 0u)));
                v430 = select(v350, v428, v429);
                v431 = f64_lt(vec2<u32>(4277811695u, 1082535490u), v350);
                v432 = select(v430, vec2<u32>(0u, 2146435072u), v431);
                v433 = f64_lt(v350, vec2<u32>(3576508497u, 3230091536u));
                v434 = select(v432, vec2<u32>(0u, 0u), v433);
                v435 = f64_mul(v8, v349);
                v436 = vec2<u32>(v435.x, v435.y & 0x7fffffffu);
                v437 = !f64_lt(v436, vec2<u32>(0u, 1121976320u));
                v438 = f64_fma(v436, vec2<u32>(1841940611u, 1071931184u), vec2<u32>(0u, 1071644672u));
                v439 = f64_trunc(v438);
                v440 = f64_mul(v439, vec2<u32>(1413754136u, 3220775419u));
                v441 = vec2<u32>(v440.x, v440.y ^ 0x80000000u);
                v442 = f64_fma(v439, vec2<u32>(1413754136u, 3220775419u), v441);
                v443 = f64_mul(v439, vec2<u32>(856972294u, 3163661862u));
                v444 = vec2<u32>(v443.x, v443.y ^ 0x80000000u);
                v445 = f64_fma(v439, vec2<u32>(856972294u, 3163661862u), v444);
                v446 = f64_mul(v439, vec2<u32>(688016904u, 3108773073u));
                v447 = vec2<u32>(v446.x, v446.y ^ 0x80000000u);
                v448 = f64_fma(v439, vec2<u32>(688016904u, 3108773073u), v447);
                v449 = f64_add(v443, v442);
                v450 = f64_sub(v449, v442);
                v451 = f64_sub(v443, v450);
                v452 = f64_add(v446, v451);
                v453 = f64_add(v445, v452);
                v454 = f64_add(v448, v453);
                v455 = f64_add(v440, v449);
                v456 = f64_sub(v455, v440);
                v457 = f64_sub(v449, v456);
                v458 = f64_add(v457, v454);
                v459 = f64_sub(v458, v457);
                v460 = f64_sub(v454, v459);
                v461 = f64_add(v436, v455);
                v462 = f64_add(v461, v458);
                v463 = f64_sub(v462, v461);
                v464 = f64_sub(v458, v463);
                v465 = f64_add(v460, v464);
                v466 = f64_to_i64(v439);
                v467 = v466.x;
                v468 = v436;
                v469 = lshr64(v468, vec2<u32>(52u, 0u).x);
                v470 = v469.x;
                v471 = v470 + 4294966273u;
                v472 = u32(bitcast<i32>(v471) >> 3u);
                v473 = u32(max(bitcast<i32>(v472), bitcast<i32>(6u)));
                v474 = 133u - v473;
                v475 = v474 & 240u;
                v476 = 4096u + v475;
                v477 = vec2<u32>(glob6[((v476) - 4096u) >> 2u], glob6[(((v476 + 4u)) - 4096u) >> 2u]);
                v478 = (4096u + 8u) + v475;
                v479 = vec2<u32>(glob6[((v478) - 4096u) >> 2u], glob6[(((v478 + 4u)) - 4096u) >> 2u]);
                v480 = (4096u + 16u) + v475;
                v481 = vec2<u32>(glob6[((v480) - 4096u) >> 2u], glob6[(((v480 + 4u)) - 4096u) >> 2u]);
                v482 = (4096u + 24u) + v475;
                v483 = vec2<u32>(glob6[((v482) - 4096u) >> 2u], glob6[(((v482 + 4u)) - 4096u) >> 2u]);
                v484 = (4096u + 32u) + v475;
                v485 = vec2<u32>(glob6[((v484) - 4096u) >> 2u], glob6[(((v484 + 4u)) - 4096u) >> 2u]);
                v486 = v477.x;
                v487 = lshr64(v477, vec2<u32>(32u, 0u).x);
                v488 = v487.x;
                v489 = v479.x;
                v490 = lshr64(v479, vec2<u32>(32u, 0u).x);
                v491 = v490.x;
                v492 = v481.x;
                v493 = lshr64(v481, vec2<u32>(32u, 0u).x);
                v494 = v493.x;
                v495 = v483.x;
                v496 = lshr64(v483, vec2<u32>(32u, 0u).x);
                v497 = v496.x;
                v498 = v485.x;
                v499 = lshr64(v485, vec2<u32>(32u, 0u).x);
                v500 = v499.x;
                v501 = v474 >> 2u;
                v502 = v501 & 3u;
                v503 = v502 == 1u;
                v504 = v502 == 2u;
                v505 = v502 == 3u;
                v506 = select(v486, v488, v503);
                v507 = select(v506, v489, v504);
                v508 = select(v507, v491, v505);
                v509 = select(v488, v489, v503);
                v510 = select(v509, v491, v504);
                v511 = select(v510, v492, v505);
                v512 = select(v489, v491, v503);
                v513 = select(v512, v492, v504);
                v514 = select(v513, v494, v505);
                v515 = select(v491, v492, v503);
                v516 = select(v515, v494, v504);
                v517 = select(v516, v495, v505);
                v518 = select(v492, v494, v503);
                v519 = select(v518, v495, v504);
                v520 = select(v519, v497, v505);
                v521 = select(v494, v495, v503);
                v522 = select(v521, v497, v504);
                v523 = select(v522, v498, v505);
                v524 = select(v495, v497, v503);
                v525 = select(v524, v498, v504);
                v526 = select(v525, v500, v505);
                v527 = vec2<u32>(v511, 0u);
                v528 = shl64(v527, vec2<u32>(32u, 0u).x);
                v529 = vec2<u32>(v508, 0u);
                v530 = v528 | v529;
                v531 = v474 << 3u;
                v532 = v531 & 24u;
                v533 = vec2<u32>(v532, 0u);
                v534 = ashr64(v530, v533.x);
                v535 = vec2<u32>(v514, 0u);
                v536 = shl64(v535, vec2<u32>(32u, 0u).x);
                v537 = v536 | v527;
                v538 = ashr64(v537, v533.x);
                v539 = v538.x;
                v540 = vec2<u32>(v517, 0u);
                v541 = shl64(v540, vec2<u32>(32u, 0u).x);
                v542 = v541 | v535;
                v543 = ashr64(v542, v533.x);
                v544 = v543.x;
                v545 = vec2<u32>(v520, 0u);
                v546 = shl64(v545, vec2<u32>(32u, 0u).x);
                v547 = v546 | v540;
                v548 = ashr64(v547, v533.x);
                v549 = vec2<u32>(v523, 0u);
                v550 = shl64(v549, vec2<u32>(32u, 0u).x);
                v551 = v550 | v545;
                v552 = ashr64(v551, v533.x);
                v553 = v552.x;
                v554 = vec2<u32>(v526, 0u);
                v555 = shl64(v554, vec2<u32>(32u, 0u).x);
                v556 = v555 | v549;
                v557 = ashr64(v556, v533.x);
                v558 = v557.x;
                v559 = bitcast<i32>(v472) < bitcast<i32>(111u);
                v560 = select(vec2<u32>(0u, 930086912u), vec2<u32>(0u, 1072693248u), v559);
                v561 = f64_mul(v436, v560);
                v562 = select(142606336u, 0u, v559);
                v563 = v473 << 23u;
                v564 = v562 - v563;
                v565 = v564 + 982515712u;
                v566 = vec2<u32>(v565, 0u);
                v567 = shl64(v566, vec2<u32>(32u, 0u).x);
                v568 = v567;
                v569 = v534 & vec2<u32>(4294967295u, 0u);
                v570 = v539 & 65535u;
                v571 = v570 | v565;
                v572 = vec2<u32>(v571, 0u);
                v573 = shl64(v572, vec2<u32>(32u, 0u).x);
                v574 = v573 | v569;
                v575 = v574;
                v576 = f64_sub(v575, v568);
                v577 = v564 + 1032847360u;
                v578 = vec2<u32>(v577, 0u);
                v579 = shl64(v578, vec2<u32>(32u, 0u).x);
                v580 = v579;
                v581 = select(((v544 << (16u % 32u)) | (v539 >> (32u - (16u % 32u)))) & 4294967295u, v544, (16u % 32u) == 0u);
                v582 = vec2<u32>(v581, 0u);
                v583 = v544 >> 16u;
                v584 = v583 | v577;
                v585 = vec2<u32>(v584, 0u);
                v586 = shl64(v585, vec2<u32>(32u, 0u).x);
                v587 = v586 | v582;
                v588 = v587;
                v589 = f64_sub(v588, v580);
                v590 = v564 + 1083179008u;
                v591 = vec2<u32>(v590, 0u);
                v592 = v548 & vec2<u32>(4294967295u, 0u);
                v593 = v553 & 65535u;
                v594 = v593 | v590;
                v595 = vec2<u32>(v594, 0u);
                v596 = v564 + 1133510656u;
                v597 = vec2<u32>(v596, 0u);
                v598 = select(((v558 << (16u % 32u)) | (v553 >> (32u - (16u % 32u)))) & 4294967295u, v558, (16u % 32u) == 0u);
                v599 = vec2<u32>(v598, 0u);
                v600 = v558 >> 16u;
                v601 = v600 | v596;
                v602 = vec2<u32>(v601, 0u);
                v603 = f64_mul(v561, v576);
                v604 = vec2<u32>(v603.x, v603.y ^ 0x80000000u);
                v605 = f64_fma(v576, v561, v604);
                v606 = f64_mul(v561, v589);
                v607 = vec2<u32>(v606.x, v606.y ^ 0x80000000u);
                v608 = f64_fma(v589, v561, v607);
                v609 = array<vec2<u32>, 2>(v591, array<vec2<u32>, 2>()[1]);
                v610 = array<vec2<u32>, 2>(v609[0], v597);
                v611[0] = shl64(v610[0], array<vec2<u32>, 2>(vec2<u32>(32u, 0u), vec2<u32>(32u, 0u))[0].x);
                v611[1] = shl64(v610[1], array<vec2<u32>, 2>(vec2<u32>(32u, 0u), vec2<u32>(32u, 0u))[1].x);
                v612[0] = v611[0];
                v612[1] = v611[1];
                v613 = array<vec2<u32>, 2>(v595, array<vec2<u32>, 2>()[1]);
                v614 = array<vec2<u32>, 2>(v613[0], v602);
                v615[0] = shl64(v614[0], array<vec2<u32>, 2>(vec2<u32>(32u, 0u), vec2<u32>(32u, 0u))[0].x);
                v615[1] = shl64(v614[1], array<vec2<u32>, 2>(vec2<u32>(32u, 0u), vec2<u32>(32u, 0u))[1].x);
                v616 = array<vec2<u32>, 2>(v592, array<vec2<u32>, 2>()[1]);
                v617 = array<vec2<u32>, 2>(v616[0], v599);
                v618[0] = v615[0] | v617[0];
                v618[1] = v615[1] | v617[1];
                v619[0] = v618[0];
                v619[1] = v618[1];
                v620[0] = f64_sub(v619[0], v612[0]);
                v620[1] = f64_sub(v619[1], v612[1]);
                v621 = array<vec2<u32>, 2>(v561, array<vec2<u32>, 2>()[1]);
                v622 = array<vec2<u32>, 2>(v621[0], v621[0]);
                v623[0] = f64_mul(v622[0], v620[0]);
                v623[1] = f64_mul(v622[1], v620[1]);
                v624 = v623[0];
                v625 = vec2<u32>(v624.x, v624.y ^ 0x80000000u);
                v626 = v620[0];
                v627 = f64_fma(v626, v561, v625);
                v628 = v623[1];
                v629 = vec2<u32>(v628.x, v628.y ^ 0x80000000u);
                v630 = v620[1];
                v631 = f64_fma(v630, v561, v629);
                v632 = f64_add(v624, v628);
                v633 = f64_sub(v632, v628);
                v634 = f64_sub(v624, v633);
                v635 = f64_add(v631, v634);
                v636 = f64_sub(v635, v631);
                v637 = f64_sub(v634, v636);
                v638 = f64_add(v606, v635);
                v639 = f64_sub(v638, v635);
                v640 = f64_sub(v606, v639);
                v641 = f64_add(v637, v640);
                v642 = f64_add(v627, v641);
                v643 = f64_sub(v642, v627);
                v644 = f64_sub(v641, v643);
                v645 = f64_add(v603, v642);
                v646 = f64_sub(v645, v642);
                v647 = f64_sub(v603, v646);
                v648 = f64_add(v644, v647);
                v649 = f64_add(v608, v648);
                v650 = f64_add(v605, v649);
                v651 = f64_mul(v632, vec2<u32>(0u, 1062207488u));
                v652 = f64_floor(v651);
                v653 = f64_sub(v651, v652);
                v654 = f64_min(v653, vec2<u32>(4294967295u, 1072693247u));
                v655 = vec2<u32>(v651.x, v651.y & 0x7fffffffu);
                v656 = !f64_eq(v655, vec2<u32>(0u, 2146435072u));
                v657 = select(vec2<u32>(0u, 0u), v654, v656);
                v658 = !(f64_is_nan(v651) | f64_is_nan(vec2<u32>(0u, 0u)));
                v659 = select(v651, v657, v658);
                v660 = f64_mul(v659, vec2<u32>(0u, 1083179008u));
                v661 = f64_add(v638, v660);
                v662 = f64_lt(v661, vec2<u32>(0u, 0u));
                v663 = select(vec2<u32>(0u, 0u), vec2<u32>(0u, 1083179008u), v662);
                v664 = f64_add(v660, v663);
                v665 = f64_add(v638, v664);
                v666 = f64_floor(v665);
                v667 = f64_sub(v665, v666);
                v668 = f64_min(v667, vec2<u32>(4294967295u, 1072693247u));
                v669 = vec2<u32>(v665.x, v665.y & 0x7fffffffu);
                v670 = !f64_eq(v669, vec2<u32>(0u, 2146435072u));
                v671 = select(vec2<u32>(0u, 0u), v668, v670);
                v672 = !(f64_is_nan(v665) | f64_is_nan(vec2<u32>(0u, 0u)));
                v673 = select(v665, v671, v672);
                v674 = f64_sub(v665, v673);
                v675 = f64_to_f32(v674);
                v676 = u32(i32(v675));
                v677 = f64_sub(v664, v674);
                v678 = f64_add(v638, v677);
                v679 = f64_sub(v678, v677);
                v680 = f64_sub(v638, v679);
                v681 = f64_add(v645, v680);
                v682 = f64_sub(v681, v680);
                v683 = f64_sub(v645, v682);
                v684 = f64_add(v650, v683);
                v685 = (f64_lt(vec2<u32>(0u, 1071644672u), v678) | f64_eq(v678, vec2<u32>(0u, 1071644672u)));
                v686 = select(0u, 1u, v685);
                v687 = v686 + v676;
                v688 = f64_from_u64(vec2<u32>(select(0u, 1u, v685), 0u));
                v689 = f64_sub(v678, v688);
                v690 = f64_add(v689, v681);
                v691 = f64_sub(v690, v689);
                v692 = f64_sub(v681, v691);
                v693 = f64_add(v692, v684);
                v694 = f64_mul(v690, vec2<u32>(1413754136u, 1073291771u));
                v695 = vec2<u32>(v694.x, v694.y ^ 0x80000000u);
                v696 = f64_fma(v690, vec2<u32>(1413754136u, 1073291771u), v695);
                v697 = f64_fma(v690, vec2<u32>(856972294u, 1016178214u), v696);
                v698 = f64_fma(v693, vec2<u32>(1413754136u, 1073291771u), v697);
                v699 = f64_add(v694, v698);
                v700 = f64_sub(v699, v694);
                v701 = f64_sub(v698, v700);
                v702 = select(v462, v699, v437);
                v703 = select(v465, v701, v437);
                v704 = select(v467, v687, v437);
                v705 = f64_mul(v702, v702);
                v706 = f64_mul(v705, vec2<u32>(0u, 1071644672u));
                v707 = f64_sub(vec2<u32>(0u, 1072693248u), v706);
                v708 = f64_fma(v705, vec2<u32>(4188289976u, 1038475442u), vec2<u32>(3023036327u, 3193628160u));
                v709 = f64_fma(v708, v705, vec2<u32>(2037177857u, 1053236707u));
                v710 = f64_fma(v709, v705, vec2<u32>(434650716u, 3207201184u));
                v711 = f64_fma(v710, v705, vec2<u32>(286329779u, 1065423121u));
                v712 = f64_fma(v705, vec2<u32>(1187798594u, 3181971419u), vec2<u32>(2419567480u, 1042411190u));
                v713 = f64_fma(v712, v705, vec2<u32>(2709480950u, 3197271631u));
                v714 = f64_fma(v713, v705, vec2<u32>(435481744u, 1056571808u));
                v715 = f64_fma(v714, v705, vec2<u32>(381774183u, 3210133868u));
                v716 = f64_fma(v715, v705, vec2<u32>(1431655765u, 1067799893u));
                v717 = f64_mul(v705, v705);
                v718 = f64_sub(vec2<u32>(0u, 1072693248u), v707);
                v719 = f64_sub(v718, v706);
                v720 = f64_fma(v702, v703, v719);
                v721 = f64_fma(v716, v717, v720);
                v722 = f64_add(v707, v721);
                v723 = vec2<u32>(v702.x, v702.y ^ 0x80000000u);
                v724 = f64_mul(v705, v723);
                v725 = f64_mul(v703, vec2<u32>(0u, 1071644672u));
                v726 = f64_fma(v724, v711, v725);
                v727 = vec2<u32>(v703.x, v703.y ^ 0x80000000u);
                v728 = f64_fma(v726, v705, v727);
                v729 = f64_fma(v724, vec2<u32>(1431655765u, 3217380693u), v728);
                v730 = f64_sub(v702, v729);
                v731 = vec2<u32>(v730.x, v730.y ^ 0x80000000u);
                v732 = v704 & 1u;
                v733 = v732 == 0u;
                v734 = select(v731, v722, v733);
                v735 = v734;
                v736 = v704 >> 1u;
                v737 = v736 & 1u;
                v738 = vec2<u32>(v737, 0u);
                v739 = shl64(v738, vec2<u32>(63u, 0u).x);
                v740 = v739 ^ v735;
                v741 = vec2<u32>(v435.x, v435.y & 0x7fffffffu);
                v742 = ((f64_is_nan(v741) | f64_is_nan(vec2<u32>(0u, 2146435072u))) | f64_eq(v741, vec2<u32>(0u, 2146435072u)));
                v743 = v740;
                v744 = select(v743, vec2<u32>(0u, 2146959360u), v742);
                v745 = select(v722, v730, v733);
                v746 = v745;
                v747 = f64_lt(v435, vec2<u32>(0u, 0u));
                v748 = vec2<u32>(select(0u, 1u, v747), 0u);
                v749 = v738 ^ v748;
                v750 = shl64(v749, vec2<u32>(63u, 0u).x);
                v751 = v750 ^ v746;
                v752 = v751;
                v753 = select(v752, vec2<u32>(0u, 2146959360u), v742);
                v754 = f64_mul(v434, v744);
                v755 = a1 + v5 * 8u;
                buf1[((v755) - 4096u) >> 2u] = v754.x;
                buf1[(((v755 + 4u)) - 4096u) >> 2u] = v754.y;
                v756 = f64_mul(v434, v753);
                v757 = v2 + v5 * 8u;
                buf1[((v757) - 4096u) >> 2u] = v756.x;
                buf1[(((v757 + 4u)) - 4096u) >> 2u] = v756.y;
                v758 = select(1u, args[3u + min(0u, 2u)], 0u < 3u);
                v3 = v758 + v4;
                v759 = bitcast<i32>(v3) < bitcast<i32>(16u);
                if (v759) {
                    v4 = v3;
                } else {
                    jump = 18u; break;
                }
            }
            if (jump != 0u) {
                if (jump == 18u) { jump = 0u; break; }
            }
        } else {
        }
        break;
    }
    return;
}
